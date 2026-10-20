/**
 * Web build entry (`npm run build:web` bundles this file instead of `main.ts`).
 *
 * Nothing happens in this file — the order of the two imports is the entire
 * contract: bridge first, then the unchanged desktop UI.
 */

import "./install";
import "../main";
