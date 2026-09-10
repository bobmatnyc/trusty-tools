import type { Message } from '../stores/app';
import DOMPurify from 'dompurify';
import { renderMarkdown } from './fileRendering';

/** Chat formatting is display-only: no navigation, remote resources or author UI controls. */
export function renderChatMarkdown(source: string): string {
  return DOMPurify.sanitize(renderMarkdown(source), {
    ALLOWED_TAGS: ['p', 'br', 'hr', 'strong', 'b', 'em', 'i', 'del', 's', 'blockquote', 'pre', 'code', 'ul', 'ol', 'li', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'table', 'thead', 'tbody', 'tfoot', 'tr', 'th', 'td', 'span', 'div'],
    ALLOWED_ATTR: ['colspan', 'rowspan', 'start'],
    ALLOW_DATA_ATTR: false,
    ALLOW_ARIA_ATTR: false,
  });
}

/** Tool evidence belongs to a response only when its task identifier matches exactly. */
export function assistantEmptyNotice(message: Message, conversation: Message[]): string {
  const activities = message.taskId ? conversation.filter(item => item.role === 'tool' && item.taskId === message.taskId) : [];
  if (activities.some(item => item.activityStatus === 'error')) return 'No response was returned. One or more tool activities failed.';
  if (activities.length && activities.every(item => item.activityStatus === 'complete')) return 'Tool activities completed. No written response was returned.';
  return 'No response was returned.';
}
