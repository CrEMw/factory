// Last seen agent mailbox message id in the browser (the "new" divider on /agents).
export const SEEN_KEY = 'hub:agents:seen';

type Store = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

export function readSeen(store: Store): number {
  return Number(store.getItem(SEEN_KEY)) || 0;
}

export function writeSeen(store: Store, id: number): void {
  store.setItem(SEEN_KEY, String(id));
}
