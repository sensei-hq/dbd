// Canonical site origin (production). Used for canonical links, og:url, and the
// sitemap so they're correct regardless of the host the build is previewed on.
export const SITE_URL = 'https://dbd.sensei-hq.com';

/** Default social share image (absolute URL). */
export const OG_IMAGE = `${SITE_URL}/favicon/android-chrome-512x512.png`;

/** Absolute canonical URL for a pathname — trailing slash normalized away (except root). */
export function canonicalFor(pathname: string): string {
	const p = pathname !== '/' ? pathname.replace(/\/+$/, '') : '/';
	return SITE_URL + p;
}
