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

/** Every page a crawler should index, for the sitemap. /projects is deliberately
 *  absent: it is a personal, localStorage-backed workspace with nothing to crawl
 *  (it is noindex, and robots.txt disallows it). */
export function indexableRoutes(guideSlugs: string[]): string[] {
	return ['/', '/guide', '/diagram', '/changelog', ...guideSlugs.map((slug) => `/guide/${slug}`)];
}
