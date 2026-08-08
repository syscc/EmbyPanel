export type ApiClient = <T>(path: string, init?: RequestInit) => Promise<T>

export function createApiClient(options: {
  getToken: () => string
  onUnauthorized: (path: string, requestToken: string) => void
}): ApiClient {
  return async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
    const requestToken = options.getToken()
    const headers = new Headers(init.headers)
    headers.set('Content-Type', 'application/json')
    if (requestToken) headers.set('Authorization', `Bearer ${requestToken}`)
    const response = await fetch(path, { ...init, headers })
    if (!response.ok) {
      const message = await response.text()
      if (response.status === 401 && requestToken && !isAuthBootstrapPath(path)) {
        options.onUnauthorized(path, requestToken)
      }
      throw new Error(message)
    }
    return response.json() as Promise<T>
  }
}

function isAuthBootstrapPath(path: string) {
  return path === '/api/login' || path === '/api/setup' || path === '/api/setup-status'
}
