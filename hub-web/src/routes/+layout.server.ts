import { hubName } from '#lib/server/auth';
import { locale } from '#lib/server/locale';
import type { LayoutServerLoad } from './$types';

// Hub name for the header and titles (step 2.8) and UI locale (step 2.9): from server env, so one image serves any
// installation. The root layout sets the client locale from `locale`.
export const load: LayoutServerLoad = () => ({ hubName, locale });
