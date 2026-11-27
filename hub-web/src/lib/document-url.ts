export function documentHref(repo: string, path: string): string {
  return `/d/${path.split('/').map(encodeURIComponent).join('/')}?repo=${encodeURIComponent(repo)}`;
}
