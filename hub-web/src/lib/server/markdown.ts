import MarkdownIt from 'markdown-it';

// html: false: raw HTML in ticket text is shown as text, not executed.
// markdown-it itself drops javascript:, vbscript:, file: and data: links (except images).
const md = new MarkdownIt({ html: false, linkify: true, breaks: false });

// External links open in a new tab and do not get window.opener.
const defaultLink = md.renderer.rules.link_open ?? ((tokens, i, opts, _env, self) => self.renderToken(tokens, i, opts));
md.renderer.rules.link_open = (tokens, i, opts, env, self) => {
  const href = String(tokens[i].attrGet('href') ?? '');
  if (/^https?:/i.test(href)) {
    tokens[i].attrSet('target', '_blank');
    tokens[i].attrSet('rel', 'noopener noreferrer');
  }
  return defaultLink(tokens, i, opts, env, self);
};

export function renderMarkdown(text: string): string {
  return md.render(text);
}
