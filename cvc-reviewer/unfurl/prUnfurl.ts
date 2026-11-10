// Link previews ("unfurls") for Reviewer pull request links on the hosted deployment.
//
// Slack, X and LinkedIn read a link's Open Graph and Twitter card tags from the HTML the server
// sends. They do not run JavaScript, so tags this SPA set at runtime would never be seen, and
// middleware.ts adds them on the server instead, to /pr/:owner/:repo/:pr only.
//
// A preview says only what the link already says: the owner, the repository, the pull request
// number, and a thought count when the link carries one. Its image is the platform's public card
// (GET <platform>/api/og), which looks the pull request up on GitHub anonymously. CVC content
// (prompts, reasoning, code) never goes into a preview: once a service has cached a preview, a
// tombstone cannot recall it.

export const SITE_NAME = 'Volute CVC Reviewer'

// The size the platform renders the card at.
const IMAGE_WIDTH = 1200
const IMAGE_HEIGHT = 630

// The platform's /api/og answers 400 to a value that breaks these rules, so a link that breaks
// them gets no preview.
const OWNER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_-]{0,38}$/
const REPO_PATTERN = /^[A-Za-z0-9._-]{1,100}$/
const PR_NUMBER_PATTERN = /^[1-9]\d{0,8}$/
const THOUGHT_COUNT_PATTERN = /^\d{1,6}$/

// The SPA's route, /pr/:owner/:repo/:pr, with the trailing slash its router also accepts.
const PR_PATH_PATTERN = /^\/pr\/([^/]+)\/([^/]+)\/([^/]+)\/?$/

const HEAD_END_PATTERN = /<\/head\s*>/i

const SHELL_TIMEOUT_MS = 3000

export type Env = Readonly<Record<string, string | undefined>>

export type PrLink = {
  owner: string
  repo: string
  pr: string
  /** A canonical decimal count, or null when the link carries no valid one. */
  thoughtCount: string | null
}

export type UnfurlConfig = {
  /** VITE_PLATFORM_API_URL without its trailing slash, for example https://www.cvc.dev */
  platformBase: string
}

export type UnfurlOptions = {
  fetch?: typeof fetch
  timeoutMs?: number
}

const HTML_ESCAPES: Readonly<Record<string, string>> = {
  '&': '&amp;',
  '<': '&lt;',
  '>': '&gt;',
  '"': '&quot;',
  "'": '&#39;',
}

export function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => HTML_ESCAPES[character])
}

/**
 * The platform to take the card from, or null when previews are off. They are a hosted
 * integration, so they follow the rule the rest of the app does: on only when the deployment
 * sets VITE_HOSTED_MODE=true and provides VITE_PLATFORM_API_URL. A self-hosted or local PAT
 * deployment sets neither and serves its pages unchanged, and so does a deployment whose URL is
 * not a plain http(s) address.
 */
export function readUnfurlConfig(env: Env): UnfurlConfig | null {
  if (env.VITE_HOSTED_MODE !== 'true') {
    return null
  }

  const raw = env.VITE_PLATFORM_API_URL?.trim()
  if (!raw) {
    return null
  }

  let url: URL
  try {
    url = new URL(raw)
  } catch {
    return null
  }

  if (
    (url.protocol !== 'https:' && url.protocol !== 'http:') ||
    url.username !== '' ||
    url.password !== '' ||
    url.search !== '' ||
    url.hash !== ''
  ) {
    return null
  }

  return { platformBase: `${url.origin}${url.pathname.replace(/\/+$/, '')}` }
}

export function isPrRef(owner: string, repo: string, pr: string): boolean {
  return (
    OWNER_PATTERN.test(owner) &&
    REPO_PATTERN.test(repo) &&
    repo !== '.' &&
    repo !== '..' &&
    PR_NUMBER_PATTERN.test(pr)
  )
}

/**
 * Reads a pull request link. Returns null for any other path, and for a value the platform
 * would refuse; a percent-encoded value always is one. A `thoughtCount` in the query string
 * that is not a count is dropped rather than costing the link its preview.
 */
export function parsePrLink(url: URL): PrLink | null {
  const match = PR_PATH_PATTERN.exec(url.pathname)
  if (!match) {
    return null
  }

  const [, owner, repo, pr] = match
  if (!isPrRef(owner, repo, pr)) {
    return null
  }

  const rawCount = url.searchParams.get('thoughtCount')
  const thoughtCount =
    rawCount !== null && THOUGHT_COUNT_PATTERN.test(rawCount) ? String(Number(rawCount)) : null

  return { owner, repo, pr, thoughtCount }
}

/** The tags for one pull request link, every value HTML-escaped, one per line. */
export function buildPrUnfurlTags(pageOrigin: string, link: PrLink, config: UnfurlConfig): string {
  const { owner, repo, pr, thoughtCount } = link
  const name = `${owner}/${repo}`
  const title = `${name} #${pr} · ${SITE_NAME}`
  const description = `Review the captured coding context of ${name} pull request #${pr} alongside its diff.`
  const imageAlt = `Preview card for ${name} pull request #${pr}`

  const imageQuery = new URLSearchParams({ owner, repo, pr })
  if (thoughtCount !== null) {
    imageQuery.set('thoughtCount', thoughtCount)
  }
  const image = `${config.platformBase}/api/og?${imageQuery}`

  const pagePath = [owner, repo, pr].map(encodeURIComponent).join('/')
  const pageQuery = thoughtCount === null ? '' : `?${new URLSearchParams({ thoughtCount })}`
  const pageUrl = `${pageOrigin}/pr/${pagePath}${pageQuery}`

  const tags: ReadonlyArray<readonly ['property' | 'name', string, string]> = [
    ['property', 'og:type', 'website'],
    ['property', 'og:site_name', SITE_NAME],
    ['property', 'og:title', title],
    ['property', 'og:description', description],
    ['property', 'og:url', pageUrl],
    ['property', 'og:image', image],
    ['property', 'og:image:width', String(IMAGE_WIDTH)],
    ['property', 'og:image:height', String(IMAGE_HEIGHT)],
    ['property', 'og:image:alt', imageAlt],
    ['name', 'twitter:card', 'summary_large_image'],
    ['name', 'twitter:title', title],
    ['name', 'twitter:description', description],
    ['name', 'twitter:image', image],
    ['name', 'twitter:image:alt', imageAlt],
  ]

  return tags
    .map(([attribute, key, content]) => `    <meta ${attribute}="${key}" content="${escapeHtml(content)}" />\n`)
    .join('')
}

/** The page with `markup` added just before its first `</head>`, or null when it has none. */
export function injectIntoHead(html: string, markup: string): string | null {
  const headEnd = HEAD_END_PATTERN.exec(html)
  if (!headEnd) {
    return null
  }

  return html.slice(0, headEnd.index) + markup + html.slice(headEnd.index)
}

/**
 * The built index.html with this pull request's tags, or null to serve the request unchanged:
 * previews off, not a GET, not a valid pull request link, or no usable page to add them to.
 */
export async function unfurlResponse(
  request: Request,
  env: Env,
  options: UnfurlOptions = {},
): Promise<Response | null> {
  if (request.method !== 'GET') {
    return null
  }

  const config = readUnfurlConfig(env)
  if (config === null) {
    return null
  }

  const pageUrl = new URL(request.url)
  const link = parsePrLink(pageUrl)
  if (link === null) {
    return null
  }

  const shell = await fetchShell(new URL('/index.html', pageUrl), options)
  if (shell === null) {
    return null
  }

  const html = injectIntoHead(shell, buildPrUnfurlTags(pageUrl.origin, link, config))
  if (html === null) {
    return null
  }

  return new Response(html, {
    status: 200,
    headers: {
      'content-type': 'text/html; charset=utf-8',
      // What the CDN sends with the static index.html.
      'cache-control': 'public, max-age=0, must-revalidate',
    },
  })
}

// This deployment's own index.html, asked for anonymously: the visitor's cookies and
// authorization are not passed on, and a redirect (to the login page in front of a protected
// preview deployment, say) is not followed. Anything but a 200 HTML answer in time means no
// preview.
async function fetchShell(url: URL, options: UnfurlOptions): Promise<string | null> {
  const fetchPage = options.fetch ?? fetch

  try {
    const response = await fetchPage(url, {
      headers: { accept: 'text/html' },
      redirect: 'manual',
      signal: AbortSignal.timeout(options.timeoutMs ?? SHELL_TIMEOUT_MS),
    })
    const type = (response.headers.get('content-type') ?? '').split(';')[0].trim().toLowerCase()
    if (response.status !== 200 || type !== 'text/html') {
      await response.body?.cancel()
      return null
    }

    return await response.text()
  } catch {
    return null
  }
}
