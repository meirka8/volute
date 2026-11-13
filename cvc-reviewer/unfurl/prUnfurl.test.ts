// @vitest-environment node
import { readFileSync } from 'node:fs'
import { afterEach, describe, expect, it, vi } from 'vitest'
import middleware, { config as middlewareConfig } from '../middleware'
import {
  buildPrUnfurlTags,
  escapeHtml,
  injectIntoHead,
  isPrRef,
  parsePrLink,
  readUnfurlConfig,
  unfurlResponse,
  type Env,
  type PrLink,
  type UnfurlConfig,
} from './prUnfurl'

const HOSTED: Env = { VITE_HOSTED_MODE: 'true', VITE_PLATFORM_API_URL: 'https://www.cvc.dev' }
const CONFIG: UnfurlConfig = { platformBase: 'https://www.cvc.dev' }
const REVIEWER = 'https://reviewer.cvc.dev'
const PAGE = `${REVIEWER}/pr/meirka8/volute/184`
const LINK: PrLink = { owner: 'meirka8', repo: 'volute', pr: '184', thoughtCount: null }
const SHELL = readFileSync(new URL('../index.html', import.meta.url), 'utf8')

const TITLE = 'meirka8/volute #184 · Volute CVC Reviewer'
const DESCRIPTION = 'Review the captured coding context of meirka8/volute pull request #184 alongside its diff.'
const IMAGE = 'https://www.cvc.dev/api/og?owner=meirka8&repo=volute&pr=184'
const IMAGE_ALT = 'Preview card for meirka8/volute pull request #184'

const EXPECTED_TAGS: Array<[string, string]> = [
  ['og:type', 'website'],
  ['og:site_name', 'Volute CVC Reviewer'],
  ['og:title', TITLE],
  ['og:description', DESCRIPTION],
  ['og:url', PAGE],
  ['og:image', IMAGE],
  ['og:image:width', '1200'],
  ['og:image:height', '630'],
  ['og:image:alt', IMAGE_ALT],
  ['twitter:card', 'summary_large_image'],
  ['twitter:title', TITLE],
  ['twitter:description', DESCRIPTION],
  ['twitter:image', IMAGE],
  ['twitter:image:alt', IMAGE_ALT],
]

const ENTITIES: Readonly<Record<string, string>> = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" }

/** The og: and twitter: tags of a page, in order, with their values unescaped. */
function previewTags(html: string): Array<[string, string]> {
  return [...html.matchAll(/<meta (?:property|name)="((?:og|twitter):[^"]*)" content="([^"]*)" \/>/g)].map(
    ([, key, content]) => [key, content.replace(/&(amp|lt|gt|quot|#39);/g, (_, entity: string) => ENTITIES[entity])],
  )
}

function htmlResponse(body: string, init: ResponseInit = {}): Response {
  return new Response(body, { status: 200, headers: { 'content-type': 'text/html; charset=utf-8' }, ...init })
}

function shellFetch(answer: () => Response | Promise<Response> = () => htmlResponse(SHELL)) {
  return vi.fn<typeof fetch>(async () => answer())
}

describe('escapeHtml', () => {
  it('escapes the characters that can end an attribute or start markup', () => {
    expect(escapeHtml(`<a title='x' href="y">&amp;</a>`)).toBe(
      '&lt;a title=&#39;x&#39; href=&quot;y&quot;&gt;&amp;amp;&lt;/a&gt;',
    )
  })

  it('leaves other text as it is', () => {
    expect(escapeHtml('meirka8/volute #184 · Volute CVC Reviewer')).toBe('meirka8/volute #184 · Volute CVC Reviewer')
  })
})

describe('readUnfurlConfig', () => {
  it('is on with hosted mode and a platform URL', () => {
    expect(readUnfurlConfig(HOSTED)).toEqual(CONFIG)
  })

  it('keeps a path prefix and drops trailing slashes', () => {
    expect(readUnfurlConfig({ ...HOSTED, VITE_PLATFORM_API_URL: ' https://example.com/cvc// ' })).toEqual({
      platformBase: 'https://example.com/cvc',
    })
    expect(readUnfurlConfig({ ...HOSTED, VITE_PLATFORM_API_URL: 'http://localhost:3000/' })).toEqual({
      platformBase: 'http://localhost:3000',
    })
  })

  it.each<[string, Env]>([
    ['nothing is set (local PAT mode)', {}],
    ['only the platform URL is set', { VITE_PLATFORM_API_URL: 'https://www.cvc.dev' }],
    ['hosted mode is not exactly "true"', { ...HOSTED, VITE_HOSTED_MODE: 'TRUE' }],
    ['the platform URL is missing', { VITE_HOSTED_MODE: 'true' }],
    ['the platform URL is blank', { ...HOSTED, VITE_PLATFORM_API_URL: '  ' }],
    ['the platform URL has no scheme', { ...HOSTED, VITE_PLATFORM_API_URL: 'www.cvc.dev' }],
    ['the platform URL is a script', { ...HOSTED, VITE_PLATFORM_API_URL: 'javascript:alert(1)' }],
    ['the platform URL is not http(s)', { ...HOSTED, VITE_PLATFORM_API_URL: 'ftp://www.cvc.dev' }],
    ['the platform URL carries credentials', { ...HOSTED, VITE_PLATFORM_API_URL: 'https://user:secret@www.cvc.dev' }],
    ['the platform URL has a query string', { ...HOSTED, VITE_PLATFORM_API_URL: 'https://www.cvc.dev/?next=1' }],
    ['the platform URL has a fragment', { ...HOSTED, VITE_PLATFORM_API_URL: 'https://www.cvc.dev/#top' }],
  ])('is off when %s', (_label, env) => {
    expect(readUnfurlConfig(env)).toBeNull()
  })
})

describe('parsePrLink', () => {
  const link = (path: string) => parsePrLink(new URL(path, REVIEWER))

  it('reads a pull request link', () => {
    expect(link('/pr/meirka8/volute/184')).toEqual(LINK)
    expect(link('/pr/meirka8/volute/184/')).toEqual(LINK)
    expect(link('/pr/my-org/repo.name_x-1/7')).toEqual({ owner: 'my-org', repo: 'repo.name_x-1', pr: '7', thoughtCount: null })
  })

  it('accepts the longest owner, repository and number the platform does', () => {
    const owner = 'a'.repeat(39)
    const repo = 'r'.repeat(100)
    expect(link(`/pr/${owner}/${repo}/999999999`)).toEqual({ owner, repo, pr: '999999999', thoughtCount: null })
  })

  it.each([
    '/app',
    '/pr',
    '/pr/meirka8/volute',
    '/pr/meirka8/volute/184/files',
    '/pull/meirka8/volute/184',
    '/PR/meirka8/volute/184',
    '/pr//volute/184',
    '/pr/-meirka8/volute/184',
    '/pr/_meirka8/volute/184',
    '/pr/meir.ka8/volute/184',
    `/pr/${'a'.repeat(40)}/volute/184`,
    `/pr/meirka8/${'r'.repeat(101)}/184`,
    '/pr/meirka8/volute/0',
    '/pr/meirka8/volute/0184',
    '/pr/meirka8/volute/1234567890',
    '/pr/meirka8/volute/-1',
    '/pr/meirka8/volute/1e3',
    '/pr/meirka8/volute/18.4',
    '/pr/%3Cscript%3E/volute/184',
    '/pr/meirka8/vol%20ute/184',
    '/pr/meirka8/%2E%2E%2Fsecret/184',
    '/pr/meirka8/volute/%31%38%34',
  ])('ignores %s', (path) => {
    expect(link(path)).toBeNull()
  })

  it('refuses the repository names "." and ".."', () => {
    // A URL parser removes such segments before a path gets here, so check the rule itself.
    expect(isPrRef('meirka8', '.', '184')).toBe(false)
    expect(isPrRef('meirka8', '..', '184')).toBe(false)
    expect(isPrRef('meirka8', '...', '184')).toBe(true)
  })

  it.each([
    ['7', '7'],
    ['007', '7'],
    ['0', '0'],
    ['999999', '999999'],
  ])('passes thoughtCount=%s on as %s', (raw, expected) => {
    expect(link(`/pr/meirka8/volute/184?thoughtCount=${raw}`)).toEqual({ ...LINK, thoughtCount: expected })
  })

  it.each(['', '1234567', '-1', '+7', '%207', '1.5', '7abc', 'seven'])('drops thoughtCount=%s', (raw) => {
    expect(link(`/pr/meirka8/volute/184?thoughtCount=${raw}`)).toEqual(LINK)
  })
})

describe('buildPrUnfurlTags', () => {
  it('builds the Open Graph and Twitter card tags of the link', () => {
    expect(previewTags(buildPrUnfurlTags(REVIEWER, LINK, CONFIG))).toEqual(EXPECTED_TAGS)
  })

  it('writes one escaped tag per line', () => {
    const markup = buildPrUnfurlTags(REVIEWER, LINK, CONFIG)
    expect(markup).toContain(
      '    <meta property="og:image" content="https://www.cvc.dev/api/og?owner=meirka8&amp;repo=volute&amp;pr=184" />\n',
    )
    expect(markup).toContain(`    <meta property="og:title" content="${TITLE}" />\n`)
    expect(markup.trimEnd().split('\n')).toHaveLength(EXPECTED_TAGS.length)
  })

  it('adds a thought count to the image and the page URL', () => {
    const tags = new Map(previewTags(buildPrUnfurlTags(REVIEWER, { ...LINK, thoughtCount: '7' }, CONFIG)))
    expect(tags.get('og:image')).toBe(`${IMAGE}&thoughtCount=7`)
    expect(tags.get('twitter:image')).toBe(`${IMAGE}&thoughtCount=7`)
    expect(tags.get('og:url')).toBe(`${PAGE}?thoughtCount=7`)
    expect(tags.get('og:title')).toBe(TITLE)
  })

  it('takes the image from the configured platform', () => {
    const tags = new Map(previewTags(buildPrUnfurlTags('http://localhost:4173', LINK, { platformBase: 'https://example.com/cvc' })))
    expect(tags.get('og:image')).toBe('https://example.com/cvc/api/og?owner=meirka8&repo=volute&pr=184')
    expect(tags.get('og:url')).toBe('http://localhost:4173/pr/meirka8/volute/184')
  })

  it('escapes every interpolated value, even ones validation would have refused', () => {
    const hostile: PrLink = { owner: '"><script>alert(1)</script>', repo: "x' onload='y", pr: '1&2', thoughtCount: '<7>' }
    const markup = buildPrUnfurlTags(REVIEWER, hostile, { platformBase: "https://example.com/a&b'c" })

    for (const line of markup.trimEnd().split('\n')) {
      expect(line).toMatch(/^ {4}<meta (property|name)="[a-z_:]+" content="[^"<>']*" \/>$/)
    }
    expect(markup).not.toMatch(/&(?!amp;|lt;|gt;|quot;|#39;)/)

    const tags = new Map(previewTags(markup))
    expect(tags.get('og:title')).toBe(`"><script>alert(1)</script>/x' onload='y #1&2 · Volute CVC Reviewer`)
    expect(tags.get('og:image')).toBe(
      "https://example.com/a&b'c/api/og?owner=%22%3E%3Cscript%3Ealert%281%29%3C%2Fscript%3E&repo=x%27+onload%3D%27y&pr=1%262&thoughtCount=%3C7%3E",
    )
  })
})

describe('injectIntoHead', () => {
  it('adds the markup just before </head>', () => {
    expect(injectIntoHead('<html><head><title>t</title></head><body></body></html>', '<meta />')).toBe(
      '<html><head><title>t</title><meta /></head><body></body></html>',
    )
  })

  it('finds the first </head> in any case', () => {
    expect(injectIntoHead('<HEAD></HEAD ><p></head></p>', 'X')).toBe('<HEAD>X</HEAD ><p></head></p>')
  })

  it('returns null for a page without </head>', () => {
    expect(injectIntoHead('<html><body></body></html>', 'X')).toBeNull()
  })

  it('leaves the rest of index.html as it is, its CSP and referrer policy included', () => {
    const markup = buildPrUnfurlTags(REVIEWER, LINK, CONFIG)
    const page = injectIntoHead(SHELL, markup)
    expect(page).not.toBeNull()
    expect(page!.replace(markup, '')).toBe(SHELL)
    expect(page!.indexOf(markup) + markup.length).toBe(page!.indexOf('</head>'))

    const csp = /<meta http-equiv="Content-Security-Policy"[^>]*>/.exec(SHELL)?.[0]
    expect(csp).toBeDefined()
    expect(page!.split(csp!)).toHaveLength(2)
    expect(page!.indexOf(csp!)).toBeLessThan(page!.indexOf(markup))
    expect(page!.split('<meta name="referrer" content="no-referrer" />')).toHaveLength(2)
  })

  it('finds no preview tags in index.html itself, which would compete with the injected ones', () => {
    expect(SHELL).not.toMatch(/og:|twitter:/)
    expect(SHELL.match(/<\/head\s*>/gi)).toHaveLength(1)
  })
})

describe('unfurlResponse', () => {
  it('serves index.html with the tags of the link', async () => {
    const fetchShell = shellFetch()
    const request = new Request(PAGE, {
      headers: { 'user-agent': 'Slackbot-LinkExpanding 1.0', cookie: 'session=secret', authorization: 'Bearer secret' },
    })

    const response = await unfurlResponse(request, HOSTED, { fetch: fetchShell })

    expect(response?.status).toBe(200)
    expect(response?.headers.get('content-type')).toBe('text/html; charset=utf-8')
    expect(response?.headers.get('cache-control')).toBe('public, max-age=0, must-revalidate')
    const page = await response!.text()
    expect(previewTags(page)).toEqual(EXPECTED_TAGS)
    expect(page).toContain('<div id="root"></div>')
    expect(page).toContain('<script type="module" src="/src/main.tsx"></script>')

    // The page is this deployment's own index.html, asked for anonymously, redirects not followed.
    expect(fetchShell).toHaveBeenCalledTimes(1)
    const [url, init] = fetchShell.mock.calls[0]
    expect(String(url)).toBe(`${REVIEWER}/index.html`)
    expect(init?.redirect).toBe('manual')
    expect(init?.signal).toBeInstanceOf(AbortSignal)
    const headers = new Headers(init?.headers)
    expect(headers.get('cookie')).toBeNull()
    expect(headers.get('authorization')).toBeNull()
  })

  it('passes the thought count of the link on to the image', async () => {
    const response = await unfurlResponse(new Request(`${PAGE}?thoughtCount=7&tab=files`), HOSTED, { fetch: shellFetch() })
    const tags = new Map(previewTags(await response!.text()))
    expect(tags.get('og:image')).toBe(`${IMAGE}&thoughtCount=7`)
    expect(tags.get('og:url')).toBe(`${PAGE}?thoughtCount=7`)
  })

  it.each<[string, Env, string, string]>([
    ['previews are off (local PAT mode)', {}, PAGE, 'GET'],
    ['hosted mode is off', { VITE_PLATFORM_API_URL: 'https://www.cvc.dev' }, PAGE, 'GET'],
    ['the platform URL is unusable', { ...HOSTED, VITE_PLATFORM_API_URL: 'app.cvc.dev' }, PAGE, 'GET'],
    ['the request is a HEAD', HOSTED, PAGE, 'HEAD'],
    ['the request is a POST', HOSTED, PAGE, 'POST'],
    ['the path is not a pull request link', HOSTED, `${REVIEWER}/pr/meirka8/volute`, 'GET'],
    ['a value is invalid', HOSTED, `${REVIEWER}/pr/meirka8/volute/0`, 'GET'],
  ])('serves the page unchanged, without asking for index.html, when %s', async (_label, env, url, method) => {
    const fetchShell = shellFetch()
    expect(await unfurlResponse(new Request(url, { method }), env, { fetch: fetchShell })).toBeNull()
    expect(fetchShell).not.toHaveBeenCalled()
  })

  // Each answer is a page the tags could go into, except for the one thing the case is about.
  const otherPage = '<html><head><title>Other</title></head><body></body></html>'

  it.each<[string, () => Response | Promise<Response>]>([
    ['the request for it fails', () => Promise.reject(new TypeError('fetch failed'))],
    ['it is missing', () => htmlResponse(otherPage, { status: 404 })],
    [
      'it redirects, as a protected preview deployment does',
      () =>
        new Response(otherPage, {
          status: 307,
          headers: { 'content-type': 'text/html; charset=utf-8', location: 'https://vercel.com/login' },
        }),
    ],
    ['it is not HTML', () => new Response(otherPage, { headers: { 'content-type': 'text/plain' } })],
    ['it has no </head>', () => htmlResponse('<p>no head</p>')],
  ])('serves the page unchanged when index.html cannot be used: %s', async (_label, answer) => {
    expect(await unfurlResponse(new Request(PAGE), HOSTED, { fetch: shellFetch(answer) })).toBeNull()
  })

  it('gives up on an index.html that does not arrive in time', async () => {
    const hanging = vi.fn<typeof fetch>(
      (_url, init) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener('abort', () => reject(init?.signal?.reason))
        }),
    )
    expect(await unfurlResponse(new Request(PAGE), HOSTED, { fetch: hanging, timeoutMs: 10 })).toBeNull()
  })
})

describe('middleware.ts', () => {
  afterEach(() => {
    vi.unstubAllEnvs()
    vi.unstubAllGlobals()
  })

  function stubHosted() {
    vi.stubEnv('VITE_HOSTED_MODE', 'true')
    vi.stubEnv('VITE_PLATFORM_API_URL', 'https://www.cvc.dev')
  }

  it('runs for pull request links only', () => {
    expect(middlewareConfig).toEqual({ matcher: '/pr/:path*' })
  })

  it('lets the request carry on to the SPA when previews are off', async () => {
    vi.stubEnv('VITE_HOSTED_MODE', undefined)
    vi.stubEnv('VITE_PLATFORM_API_URL', undefined)
    const fetchSpy = vi.fn<typeof fetch>()
    vi.stubGlobal('fetch', fetchSpy)

    const response = await middleware(new Request(PAGE))

    expect(response.headers.get('x-middleware-next')).toBe('1')
    expect(response.body).toBeNull()
    expect(fetchSpy).not.toHaveBeenCalled()
  })

  it('serves the tagged page on the hosted deployment', async () => {
    stubHosted()
    vi.stubGlobal('fetch', shellFetch())

    const response = await middleware(new Request(`${PAGE}?thoughtCount=7`))

    expect(response.headers.get('x-middleware-next')).toBeNull()
    expect(new Map(previewTags(await response.text())).get('og:image')).toBe(`${IMAGE}&thoughtCount=7`)
  })

  it('lets the request carry on when index.html cannot be had', async () => {
    stubHosted()
    vi.stubGlobal('fetch', shellFetch(() => Promise.reject(new TypeError('fetch failed'))))

    expect((await middleware(new Request(PAGE))).headers.get('x-middleware-next')).toBe('1')
  })

  it('lets the request carry on after an unexpected error', async () => {
    stubHosted()
    const broken = {
      method: 'GET',
      get url(): string {
        throw new Error('unexpected')
      },
    } as unknown as Request

    expect((await middleware(broken)).headers.get('x-middleware-next')).toBe('1')
  })
})
