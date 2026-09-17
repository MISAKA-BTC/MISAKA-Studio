// Joining a continuation onto the reply it continues.
//
// An engine that continues a turn from inside it (llama.cpp's assistant prefill) writes exactly the
// next characters. An engine given the continuation as an instruction — the 512-token mining lane —
// often starts by restating the last few words it was shown. Glued on as they come, those words
// appear twice at the seam. So the longest overlap between the end of what was already written and
// the start of what arrived is dropped, and only a real overlap: short coincidences (a single
// "の", a space) are ordinary text.

/** Shortest restatement treated as an overlap rather than a coincidence. */
const MIN_OVERLAP = 6
/** The seam is only looked for this far back; a model restating more than this is not continuing. */
const MAX_OVERLAP = 400

/** `base` followed by `addition`, without the part of `addition` that repeats the end of `base`. */
export function joinContinuation(base: string, addition: string): string {
  const limit = Math.min(MAX_OVERLAP, base.length, addition.length)
  for (let k = limit; k >= MIN_OVERLAP; k--) {
    if (base.endsWith(addition.slice(0, k))) return base + addition.slice(k)
  }
  return base + addition
}
