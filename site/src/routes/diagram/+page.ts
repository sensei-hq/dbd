// Client-only: the model lives in the URL fragment (not available during SSR)
// or is uploaded in-browser. Prerender the shell as a static SPA page.
export const prerender = true;
export const ssr = false;
