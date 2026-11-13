import { unfurlResponse } from './unfurl/prUnfurl'

// Vercel Routing Middleware (https://vercel.com/docs/routing-middleware), for pull request links
// only. A link preview's crawler does not run JavaScript, so this adds the link's Open Graph and
// Twitter card tags to the page on the server (see unfurl/prUnfurl.ts). With hosted mode off, for
// any other path, and on any failure, the request carries on unchanged to the vercel.json rewrite
// to /index.html.
export const config = {
  matcher: '/pr/:path*',
}

export default async function middleware(request: Request): Promise<Response> {
  try {
    return (await unfurlResponse(request, process.env)) ?? continueRequest()
  } catch {
    return continueRequest()
  }
}

// The response `next()` from `@vercel/functions` returns, inlined so that this one line does not
// bring that package's dependencies (@vercel/oidc, and through it execa, zod and jose) into the app.
function continueRequest(): Response {
  return new Response(null, { headers: { 'x-middleware-next': '1' } })
}
