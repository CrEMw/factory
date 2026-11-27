// Readiness probe: does not call hub, so a hub outage does not take hub-web off the Ingress.
export const GET = () => new Response('ok');
