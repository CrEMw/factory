declare global {
  namespace App {
    interface Locals {
      /** Better Auth user if the browser has a session; hub pages work without one too. */
      user: { id: string; email: string; name: string } | null;
      /** When the session was created (Unix ms): sign-in freshness for linking and mailbox keys. */
      sessionCreatedAt?: number | null;
    }
  }
}

export {};
