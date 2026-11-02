//! Opt-in research tools. No cookies, credentials, browser execution, automatic
//! redirects, environment proxies, or direct fallback from a failed Tor route.
use crate::{Error, Result, state::AppState};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use misaka_studio_core::settings::{WebMode, WebSettings};
use reqwest::{Client, Url, redirect::Policy};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_TEXT: usize = 16_000;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/search", post(search))
        .route("/fetch", post(fetch))
        .route("/check", post(check))
        .route("/open", post(open))
        .route("/browser", get(browser))
}

#[derive(Deserialize)]
struct SearchBody {
    query: String,
    // Mining consent belongs to each conversation, which the UI stores locally.
    // Missing consent must not silently enable research in a mined conversation.
    #[serde(default = "mining_default")]
    mining: bool,
}
#[derive(Deserialize)]
struct FetchBody {
    url: String,
    #[serde(default = "mining_default")]
    mining: bool,
}
fn mining_default() -> bool {
    true
}

#[derive(Serialize, Debug)]
struct Source {
    title: String,
    url: String,
    text: String,
}
#[derive(Serialize)]
struct WebResult {
    route: WebMode,
    sources: Vec<Source>,
}

async fn permitted(state: &AppState, mining: bool) -> Result<WebSettings> {
    let web = state.settings.read().await.web.clone();
    if web.mode == WebMode::Off {
        return Err(Error::bad_request("Web access is off. Enable it in Settings → Web access."));
    }
    if mining || state.backend().await.name() == "gateway" {
        return Err(Error::bad_request("Web research requires mining to be off in this conversation and a local chat model."));
    }
    Ok(web)
}

async fn search(State(state): State<Arc<AppState>>, Json(body): Json<SearchBody>) -> Result<Json<WebResult>> {
    let web = permitted(&state, body.mining).await?;
    let query = body.query.trim();
    if query.is_empty() || query.chars().count() > 500 {
        return Err(Error::bad_request("Search query must contain 1–500 characters."));
    }
    let mut page = Url::parse("https://www.bing.com/search").unwrap();
    page.query_pairs_mut().append_pair("q", query);
    open_tor(&web, &page).await?;
    ensure_tor(&web).await?;
    let mut rss = page.clone();
    rss.query_pairs_mut().append_pair("format", "rss");
    let mut sources = match download(&web, rss).await {
        Ok((_, body)) => rss_sources(&body),
        Err(_) => Vec::new(),
    };
    if sources.is_empty() {
        // Search engines sometimes block exit nodes. A provider fallback uses
        // the exact same route and never changes Tor into a direct connection.
        let mut url = Url::parse("https://html.duckduckgo.com/html/").unwrap();
        url.query_pairs_mut().append_pair("q", query);
        open_tor(&web, &url).await?;
        if let Ok((_, body)) = download(&web, url).await {
            sources = search_sources(&body);
        }
    }
    if sources.is_empty() {
        return Err(Error::bad_request(
            "Search providers returned no readable results or blocked the connection. Try another query or fetch a known URL. No direct fallback was used and no search results were invented.",
        ));
    }
    Ok(Json(WebResult { route: web.mode, sources }))
}

async fn fetch(State(state): State<Arc<AppState>>, Json(body): Json<FetchBody>) -> Result<Json<WebResult>> {
    let web = permitted(&state, body.mining).await?;
    let url = Url::parse(&body.url).map_err(|_| Error::bad_request("Enter an absolute http:// or https:// URL."))?;
    validate_url(&url, web.mode)?;
    open_tor(&web, &url).await?;
    ensure_tor(&web).await?;
    let (url, html) = download(&web, url).await?;
    let document = Html::parse_document(&html);
    let title = document
        .select(&Selector::parse("title").unwrap())
        .next()
        .map(|n| n.text().collect::<String>())
        .unwrap_or_else(|| url.to_string());
    let text = readable(&document);
    if text.trim().is_empty() {
        return Err(Error::bad_request("This page has no readable text. Pages requiring JavaScript or sign-in are not supported."));
    }
    Ok(Json(WebResult { route: web.mode, sources: vec![Source { title, url: url.to_string(), text }] }))
}

/// A deliberate external request, never a background status poll. In Tor mode
/// verify the exit with Tor Project itself; a listening SOCKS port is not proof.
async fn check(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    let web = state.settings.read().await.web.clone();
    if web.mode == WebMode::Off {
        return Err(Error::bad_request("Save a Web connection mode before testing it."));
    }
    open_tor(&web, &Url::parse("https://check.torproject.org").unwrap()).await?;
    let (_, body) = download(&web, Url::parse("https://check.torproject.org/api/ip").unwrap()).await?;
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| Error::bad_request("The connection check returned an unreadable response."))?;
    let is_tor = value["IsTor"].as_bool().unwrap_or(false);
    if web.mode == WebMode::Tor && !is_tor {
        return Err(Error::bad_request(
            "The configured SOCKS proxy did not use a Tor exit. Web requests remain configured for this proxy; no direct fallback was used.",
        ));
    }
    Ok(Json(serde_json::json!({"route": web.mode, "is_tor": is_tor, "ip": value["IP"]})))
}

// Verify the proxy is actually a Tor exit before sending a search query or URL.
// Cache only successful verification, briefly and per listener. A failed check
// never permits direct networking or a research request through an unverified proxy.
async fn ensure_tor(web: &WebSettings) -> Result<()> {
    if web.mode != WebMode::Tor {
        return Ok(());
    }
    static VERIFIED: OnceLock<tokio::sync::Mutex<Option<(String, Instant)>>> = OnceLock::new();
    let mut verified = VERIFIED.get_or_init(|| tokio::sync::Mutex::new(None)).lock().await;
    if verified.as_ref().is_some_and(|(proxy, at)| proxy == &web.tor_proxy && at.elapsed() < Duration::from_secs(60)) {
        return Ok(());
    }
    let (_, body) = download(web, Url::parse("https://check.torproject.org/api/ip").unwrap()).await?;
    require_tor_exit(&body)?;
    *verified = Some((web.tor_proxy.clone(), Instant::now()));
    Ok(())
}

fn require_tor_exit(body: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| Error::bad_request("Could not verify the Tor exit. Research was not sent; no direct fallback was used."))?;
    if value["IsTor"].as_bool() != Some(true) {
        return Err(Error::bad_request(
            "This SOCKS proxy is not a verified Tor exit. Research was not sent; no direct fallback was used.",
        ));
    }
    Ok(())
}

fn tor_proxy(web: &WebSettings) -> Result<reqwest::Proxy> {
    let address: SocketAddr = web
        .tor_proxy
        .parse()
        .map_err(|_| Error::bad_request("Tor proxy must be a loopback IP and port, such as 127.0.0.1:9050 or [::1]:9150."))?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(Error::bad_request("The Tor SOCKS proxy must listen on loopback with a nonzero port."));
    }
    reqwest::Proxy::all(format!("socks5h://{address}")).map_err(|e| Error::bad_request(e.to_string()))
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b == 18 || b == 19)))
        }
        IpAddr::V6(ip) => {
            // Only global unicast; also excludes mapped IPv4, link-local, ULA,
            // multicast and translation mechanisms that could reach private IPv4.
            let s = ip.segments();
            (s[0] & 0xe000 == 0x2000)
                && ![0x2002, 0x3ffe, 0x3fff].contains(&s[0])
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
        }
    }
}

fn validate_url(url: &Url, mode: WebMode) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.port_or_known_default(), Some(80 | 443))
    {
        return Err(Error::bad_request("Web tools accept only public HTTP/HTTPS URLs on ports 80/443, without credentials."));
    }
    let host = url.host_str().unwrap_or("").trim_end_matches('.').to_lowercase();
    // Url::host_str includes brackets for IPv6 literals.
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        if !public_ip(ip) {
            return Err(Error::bad_request("Private, loopback and reserved network addresses cannot be fetched."));
        }
    } else if !host.contains('.')
        || [".localhost", ".local", ".internal", ".home", ".test", ".invalid"].iter().any(|s| host.ends_with(s))
    {
        return Err(Error::bad_request("Local and reserved hostnames cannot be fetched."));
    }
    if host.ends_with(".onion") && mode != WebMode::Tor {
        return Err(Error::bad_request("Onion addresses require Tor mode."));
    }
    Ok(())
}

async fn client_for(web: &WebSettings, url: &Url) -> Result<Client> {
    if web.mode == WebMode::Off {
        return Err(Error::bad_request("Web access is off."));
    }
    validate_url(url, web.mode)?;
    let mut builder = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .referer(false)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(35))
        .user_agent("Mozilla/5.0 (compatible; MISAKAStudio/0.1; text research)");
    if web.mode == WebMode::Tor {
        // socks5h delegates destination DNS to the proxy. Do not call lookup_host
        // on this branch. no_proxy above disables HTTP_PROXY/NO_PROXY bypasses.
        builder = builder.proxy(tor_proxy(web)?);
    } else if url.host_str().is_some_and(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().is_err()) {
        let host = url.host_str().unwrap();
        let addresses: Vec<_> = tokio::net::lookup_host((host, url.port_or_known_default().unwrap()))
            .await
            .map_err(|_| Error::bad_request("The website's DNS lookup failed."))?
            .collect();
        if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
            return Err(Error::bad_request("The website resolves to a private or reserved address."));
        }
        // Pin the validated addresses: connecting must not perform a second,
        // potentially rebound lookup. TLS still verifies the original hostname.
        builder = builder.resolve_to_addrs(host, &addresses);
    }
    builder.build().map_err(|e| Error::bad_request(format!("Could not create Web connection: {e}")))
}

async fn download(web: &WebSettings, url: Url) -> Result<(Url, String)> {
    tokio::time::timeout(Duration::from_secs(60), download_inner(web, url))
        .await
        .map_err(|_| Error::bad_request("Web request timed out. No alternate connection was used."))?
}
async fn download_inner(web: &WebSettings, mut url: Url) -> Result<(Url, String)> {
    for _ in 0..6 {
        let client = client_for(web, &url).await?;
        let mut response = client.get(url.clone()).send().await.map_err(|_| Error::bad_request(if web.mode == WebMode::Tor {
            "Tor connection failed. Start Tor on the configured SOCKS port (9050, or Tor Browser's 9150) and retry. No direct fallback was used."
        } else { "The website could not be reached." }))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| Error::bad_request("Redirect without a valid destination."))?;
            let next = url.join(location).map_err(|_| Error::bad_request("Invalid redirect URL."))?;
            if url.scheme() == "https" && next.scheme() != "https" {
                return Err(Error::bad_request("Refusing an HTTPS-to-HTTP redirect."));
            }
            url = next;
            continue; // Every redirect gets the same URL and DNS checks.
        }
        if !response.status().is_success() {
            return Err(Error::bad_request(format!(
                "The website returned HTTP {}. It may block automated or Tor requests.",
                response.status().as_u16()
            )));
        }
        let mime = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
        if !(mime.starts_with("text/")
            || mime.starts_with("application/json")
            || mime.starts_with("application/xhtml+xml")
            || mime.starts_with("application/rss+xml")
            || mime.starts_with("application/xml"))
        {
            return Err(Error::bad_request(
                "This URL is not a supported text/HTML page. Files, PDFs and images are not downloaded by Web tools.",
            ));
        }
        if response.content_length().is_some_and(|n| n > MAX_BYTES as u64) {
            return Err(Error::bad_request("The page exceeds the 2 MB research limit."));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::bad_request("The website stopped sending the page."))? {
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Err(Error::bad_request("The page exceeds the 2 MB research limit."));
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok((url, String::from_utf8_lossy(&bytes).into_owned()));
    }
    Err(Error::bad_request("Too many redirects."))
}

fn readable(document: &Html) -> String {
    fn visit(node: ElementRef<'_>, out: &mut String) {
        if matches!(node.value().name(), "script" | "style" | "noscript" | "svg" | "template" | "head") || out.len() >= MAX_TEXT * 4 {
            return;
        }
        for child in node.children() {
            if let Some(text) = child.value().as_text() {
                out.push_str(text);
                out.push(' ');
            } else if let Some(element) = ElementRef::wrap(child) {
                visit(element, out);
            }
        }
    }
    let mut text = String::new();
    let content = document.select(&Selector::parse("main, article").unwrap()).next().unwrap_or(document.root_element());
    visit(content, &mut text);
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TEXT).collect()
}

fn rss_sources(xml: &str) -> Vec<Source> {
    #[derive(Deserialize)]
    struct Feed {
        channel: Channel,
    }
    #[derive(Deserialize)]
    struct Channel {
        #[serde(rename = "item", default)]
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        title: String,
        link: String,
        #[serde(default)]
        description: String,
    }
    let Ok(feed) = quick_xml::de::from_str::<Feed>(xml) else { return Vec::new() };
    feed.channel
        .items
        .into_iter()
        .filter_map(|item| {
            let url = Url::parse(&item.link).ok()?;
            validate_url(&url, WebMode::Tor).ok()?;
            Some(Source {
                title: item.title.chars().take(300).collect(),
                url: url.to_string(),
                text: readable(&Html::parse_fragment(&item.description)).chars().take(1500).collect(),
            })
        })
        .take(5)
        .collect()
}

fn search_sources(html: &str) -> Vec<Source> {
    let document = Html::parse_document(html);
    let rows = Selector::parse(".result").unwrap();
    let links = Selector::parse(".result__a").unwrap();
    let snippets = Selector::parse(".result__snippet").unwrap();
    let base = Url::parse("https://html.duckduckgo.com").unwrap();
    document
        .select(&rows)
        .filter_map(|row| {
            let link = row.select(&links).next()?;
            let mut url = base.join(link.value().attr("href")?).ok()?;
            if url.host_str().is_some_and(|h| h == "duckduckgo.com" || h.ends_with(".duckduckgo.com")) {
                let destination = url.query_pairs().find(|(k, _)| k == "uddg")?.1.into_owned();
                url = Url::parse(&destination).ok()?;
            }
            validate_url(&url, WebMode::Tor).ok()?;
            Some(Source {
                title: link.text().collect::<String>().chars().take(300).collect(),
                url: url.to_string(),
                text: row
                    .select(&snippets)
                    .next()
                    .map(|s| s.text().collect::<String>())
                    .unwrap_or_default()
                    .chars()
                    .take(1500)
                    .collect(),
            })
        })
        .take(5)
        .collect()
}

fn browser_path(web: &WebSettings) -> Option<PathBuf> {
    if let Some(path) = &web.tor_browser_path {
        // An explicit choice never falls through to another browser.
        return path.exists().then(|| path.clone());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![PathBuf::from("/Applications/Tor Browser.app"), home.join("Applications/Tor Browser.app")]
    } else if cfg!(windows) {
        let user = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default();
        vec![user.join("Desktop/Tor Browser/Browser/firefox.exe"), local.join("Tor Browser/Browser/firefox.exe")]
    } else {
        vec![
            home.join(".local/share/torbrowser/tbb/x86_64/tor-browser/Browser/start-tor-browser"),
            home.join("tor-browser/Browser/start-tor-browser"),
            PathBuf::from("/opt/tor-browser/Browser/start-tor-browser"),
        ]
    };
    candidates.into_iter().find(|p| p.exists())
}

async fn browser(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let path = browser_path(&state.settings.read().await.web);
    Json(serde_json::json!({ "installed": path.is_some(), "path": path }))
}

async fn open(State(state): State<Arc<AppState>>, Json(body): Json<FetchBody>) -> Result<Json<serde_json::Value>> {
    // A user clicking a citation opens Tor Browser even when automated research is off.
    let web = state.settings.read().await.web.clone();
    let url = Url::parse(&body.url).map_err(|_| Error::bad_request("Invalid page URL."))?;
    validate_url(&url, WebMode::Tor)?;
    open_tor(&web, &url).await?;
    Ok(Json(serde_json::json!({"opened": true})))
}

async fn open_tor(web: &WebSettings, url: &Url) -> Result<()> {
    let path = browser_path(web).ok_or_else(|| Error::bad_request("Tor Browser was not found. Install it from torproject.org or select its application/launcher path in Settings → Web access. No other browser was opened."))?;
    let mut command = if cfg!(target_os = "macos") {
        let mut command = tokio::process::Command::new("/usr/bin/open");
        command.arg("-a").arg(path).arg(url.as_str());
        command
    } else {
        let mut command = tokio::process::Command::new(path);
        command.arg("--new-tab").arg(url.as_str());
        command
    };
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| Error::bad_request("Could not start Tor Browser. Check its configured path. No other browser was opened."))?;
    if cfg!(target_os = "macos") {
        let status = child.wait().await.map_err(|_| Error::bad_request("Tor Browser launch failed."))?;
        if !status.success() {
            return Err(Error::bad_request("Tor Browser refused to open the tab. No other browser was opened."));
        }
    } else {
        // Browser launchers can stay alive for the entire browsing session.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn unsafe_urls_and_private_addresses_are_rejected_before_any_connection() {
        for url in [
            "file:///etc/passwd",
            "ftp://example.org/",
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://10.0.0.1/",
            "http://169.254.169.254/",
            "http://100.64.0.1/",
            "http://192.0.0.5/",
            "http://[2001:db8::1]/",
            "http://example.org:1338/",
            "https://user:secret@example.org/",
            "https://localhost/",
            "https://machine.local/",
            "https://a.localhost./",
        ] {
            assert!(validate_url(&Url::parse(url).unwrap(), WebMode::Tor).is_err(), "{url}");
        }
        assert!(validate_url(&Url::parse("https://example.org/").unwrap(), WebMode::Direct).is_ok());
        assert!(validate_url(&Url::parse("http://test.onion/").unwrap(), WebMode::Direct).is_err());
        assert!(validate_url(&Url::parse("http://test.onion/").unwrap(), WebMode::Tor).is_ok());
    }

    #[test]
    fn only_a_positive_tor_exit_verification_allows_research() {
        assert!(require_tor_exit(r#"{"IsTor":true}"#).is_ok());
        for body in [r#"{"IsTor":false}"#, r#"{"IP":"1.2.3.4"}"#, r#"{"IsTor":"true"}"#, "not JSON"] {
            assert!(require_tor_exit(body).is_err(), "{body}");
        }
    }

    #[test]
    fn only_local_socks_endpoints_are_allowed() {
        for value in ["http://localhost:9050", "example.org:9050", "192.168.1.2:9050", "127.0.0.1:0"] {
            assert!(tor_proxy(&WebSettings { tor_proxy: value.into(), ..Default::default() }).is_err());
        }
        assert!(tor_proxy(&WebSettings { tor_proxy: "[::1]:9150".into(), ..Default::default() }).is_ok());
    }

    #[test]
    fn pages_are_text_only_bounded_and_searches_keep_actual_source_links() {
        let doc = Html::parse_document(
            "<title>Title</title><main><p>Hello &amp; world</p><script>steal()</script><style>.secret{}</style><p>日本語</p></main>",
        );
        assert_eq!(readable(&doc), "Hello & world 日本語");
        let html = r#"<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F">Rust</a><div class="result__snippet">Official site</div></div><div class="result"><a class="result__a" href="http://127.0.0.1/">Bad</a></div>"#;
        let rss = rss_sources(
            r#"<rss><channel><item><title>A &amp; B</title><link>https://example.org/</link><description>Actual evidence</description></item><item><title>Private</title><link>http://127.0.0.1/</link></item></channel></rss>"#,
        );
        assert_eq!(rss.len(), 1);
        assert_eq!(rss[0].title, "A & B");
        assert_eq!(rss[0].text, "Actual evidence");
        let sources = search_sources(html);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].url, "https://www.rust-lang.org/");
        assert_eq!(sources[0].text, "Official site");
        assert_eq!(readable(&Html::parse_document(&format!("<p>{}</p>", "あ".repeat(MAX_TEXT * 2)))).chars().count(), MAX_TEXT);
    }

    // A SOCKS fixture receives the *hostname*, proving this route does not first
    // resolve it through the OS. It never contacts the public internet.
    async fn socks_fixture(response: &'static str) -> (WebSettings, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let web = WebSettings { mode: WebMode::Tor, tor_proxy: listener.local_addr().unwrap().to_string(), ..Default::default() };
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = [0; 2];
            stream.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], 5);
            let mut methods = vec![0; header[1] as usize];
            stream.read_exact(&mut methods).await.unwrap();
            stream.write_all(&[5, 0]).await.unwrap();
            let mut request = [0; 4];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [5, 1, 0, 3], "SOCKS must send a domain name, not a locally resolved IP");
            let length = stream.read_u8().await.unwrap();
            let mut domain = vec![0; length as usize];
            stream.read_exact(&mut domain).await.unwrap();
            let _port = stream.read_u16().await.unwrap();
            stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
            let mut bytes = vec![0; 4096];
            let _ = stream.read(&mut bytes).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(domain).unwrap()
        });
        (web, task)
    }

    #[tokio::test]
    async fn tor_resolves_remotely_and_returns_text_via_the_proxy() {
        let (web, task) =
            socks_fixture("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await;
        let (_, body) = download(&web, Url::parse("http://only-tor-resolves.this-does-not-exist.org/").unwrap()).await.unwrap();
        assert_eq!(body, "hello");
        assert_eq!(task.await.unwrap(), "only-tor-resolves.this-does-not-exist.org");
    }

    #[tokio::test]
    async fn redirects_cannot_reach_loopback_even_through_tor() {
        let (web, task) =
            socks_fixture("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        let error = download(&web, Url::parse("http://public.this-does-not-exist.org/").unwrap()).await.unwrap_err();
        assert!(error.to_string().contains("reserved"), "{error}");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn tor_failure_does_not_fall_back_to_direct_or_local_dns() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let web = WebSettings { mode: WebMode::Tor, tor_proxy: address.to_string(), ..Default::default() };
        let error = download(&web, Url::parse("http://only-tor-resolves.this-does-not-exist.org/").unwrap()).await.unwrap_err();
        assert!(error.to_string().contains("No direct fallback"), "{error}");
    }
}
