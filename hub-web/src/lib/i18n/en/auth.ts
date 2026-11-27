// Messages of the auth group (pages and helpers it owns). Keys start with 'auth.'; see the protocol in ../en.ts.
import type { Messages } from '#lib/i18n/types';

export default {
  // How to sign in to the hub (identity/index.ts entrances): full items are joined with 'or'.
  'auth.entrance.cloudflare': 'a Cloudflare Access account (https://{host})',
  'auth.entrance.tailnet': 'tailnet (from a user device, not a tagged one)',
  'auth.entrance.oidc': 'a {name} account',
  'auth.entrance.dev': 'the development login {login}',
  'auth.entrance.devShort': 'development login',
  'auth.entrance.or': ' or ',
  'auth.entrance.none': 'an entrance that is not configured',
  'auth.entrance.noneShort': 'no entrance configured',
  // routes/login
  'auth.login.title': 'Sign in',
  'auth.login.onlyFromMcp': 'The sign-in page opens only from an MCP client authorization.',
  'auth.login.how': 'Sign in to the hub with {how}. Open the hub that way and connect again.',
  // routes/login/fresh
  'auth.fresh.title': 'Sign in again',
  'auth.fresh.intro':
    'Linking a sign-in on People and approving mailbox keys require a sign-in no older than 5 minutes. The button creates a new session from your hub sign-in ({how}) and returns to',
  'auth.fresh.alreadyFresh': 'Your sign-in is already fresh: you can',
  'auth.fresh.goBack': 'go back',
  'auth.fresh.failed': 'Failed: {message}',
  // routes/consent
  'auth.consent.title': 'Hub access',
  'auth.consent.noRequest': 'No signed authorization request',
  'auth.consent.asks': 'asks for access to the hub tickets and documents on behalf of {name} ({email}).',
  'auth.consent.replyTo': 'The answer goes to',
  'auth.consent.localClients': 'For Claude Code and other local clients this is',
  'auth.consent.or': 'or',
  'auth.consent.otherAddress': '; any other address is a reason to deny.',
  'auth.consent.scope.openid': 'know who you are',
  'auth.consent.scope.profile': 'name',
  'auth.consent.scope.email': 'your email',
  'auth.consent.scope.offline_access': 'stay connected without signing in again',
  'auth.consent.allow': 'Allow',
  'auth.consent.deny': 'Deny',
  // lib/server/logins.ts: hub answers to the sign-in link and mailbox key forms
  'auth.logins.rollingOut': 'This feature is still rolling out: hub does not have it yet, retry after the rollout.',
  'auth.logins.freshRequired': 'A fresh sign-in is required (no older than 5 minutes): sign in again and retry.',
  'auth.logins.requiresHuman': 'Only a human with a hub session can do this.',
  'auth.logins.alreadyBound': 'Your sign-in is already linked to a person: retire the old link first.',
  'auth.logins.handleTaken': 'This person is already linked to another sign-in. A wrong link can be retired.',
  'auth.logins.unknownPerson': 'There is no such person in the directory.',
  'auth.logins.agentPerson': 'This is an agent: a sign-in links only to a human.',
  'auth.logins.retired': 'The person is retired.',
  'auth.logins.notYourLogin': 'You can link only your own sign-in.',
  'auth.logins.noLogin': 'First link your sign-in on the People page ("this is me").',
  'auth.logins.notKeyOwner':
    "The key belongs to someone else: its owner approves and revokes it; someone else's key can only be revoked while it is pending and its owner has no link.",
  'auth.logins.noSuchKey': 'There is no such key.',
  'auth.logins.needSession': 'A hub session is required: sign in and retry.',
  'auth.logins.pickPerson': 'Pick a person from the list.',
  'auth.logins.noSuchPerson': 'No such person.',
  'auth.logins.idAndStatus': 'id and status'
} satisfies Messages;
