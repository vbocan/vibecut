// Saving a file the server sends as a download.
//
// Pure helpers first (`node --test src/lib/download.test.mjs` runs their
// tests without a browser), then the one that touches the page.

/** The file name a `Content-Disposition` header gives: the exact UTF-8
 *  `filename*` when present, else the plain `filename`, else `fallback`. */
export function fileNameFromDisposition(disposition: string | null, fallback: string): string {
	const header = disposition ?? '';
	const utf8 = /filename\*\s*=\s*UTF-8''([^;\s]+)/i.exec(header)?.[1];
	if (utf8) {
		try {
			return decodeURIComponent(utf8);
		} catch {
			// Malformed percent-encoding: fall through to the plain name.
		}
	}
	const plain = /filename\s*=\s*"([^"]*)"/i.exec(header)?.[1] ?? /filename\s*=\s*([^;\s]+)/i.exec(header)?.[1];
	return plain || fallback;
}

/** Hand `blob` to the browser as a download named `fileName`. */
export function saveBlob(blob: Blob, fileName: string): void {
	const href = URL.createObjectURL(blob);
	const a = document.createElement('a');
	a.href = href;
	a.download = fileName;
	document.body.appendChild(a);
	a.click();
	a.remove();
	setTimeout(() => URL.revokeObjectURL(href), 1000);
}
