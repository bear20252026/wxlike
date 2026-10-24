/**
 * Dev harness entry (FP-G11 bootstrap).
 *
 * The real product shell (sidebar, chat, router, login) belongs to G10-01 and
 * G00-02; both are still open. Until they land, this entry mounts the G11
 * feature pages standalone so they can be built, typechecked and tested. It
 * intentionally contains no business logic and will be replaced by the G00
 * composition root — the swap is registered in
 * integration-requests/G11/G11-01.yaml.
 */

import { StrictMode, useState } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { ContentApi } from "./features/content/api";
import { ArticlesPage } from "./features/content/ArticlesPage";
import { FavoritesPage } from "./features/content/FavoritesPage";
import { MomentsPage } from "./features/content/MomentsPage";
import { StickerPanel } from "./features/content/StickerPanel";
import { FilesApi } from "./features/files/api";
import { FilesPage } from "./features/files/FilesPage";
import { VideoApi } from "./features/video/api";
import { ChannelsPage } from "./features/video/ChannelsPage";
import { CallApi } from "./features/calls/api";
import { CallPanel } from "./features/calls/CallPanel";

const content = new ContentApi();
const files = new FilesApi();
const video = new VideoApi();
const calls = new CallApi();

const TABS = ["moments", "favorites", "articles", "files", "channels", "call"] as const;
type Tab = (typeof TABS)[number];

function Harness() {
  const [tab, setTab] = useState<Tab>("moments");
  return (
    <div className="flex h-screen flex-col bg-zinc-50">
      <nav className="flex items-center gap-1 border-b border-zinc-200 bg-white px-4 py-2">
        {TABS.map((item) => (
          <button
            key={item}
            type="button"
            onClick={() => setTab(item)}
            className={`rounded-lg px-3 py-1.5 text-sm ${
              tab === item ? "bg-emerald-600 text-white" : "text-zinc-600 hover:bg-zinc-100"
            }`}
          >
            {item}
          </button>
        ))}
      </nav>
      <main className="min-h-0 flex-1">
        {tab === "moments" ? <MomentsPage ports={content} /> : null}
        {tab === "favorites" ? <FavoritesPage ports={content} /> : null}
        {tab === "articles" ? <ArticlesPage ports={content} /> : null}
        {tab === "files" ? <FilesPage ports={files} /> : null}
        {tab === "channels" ? <ChannelsPage ports={video} /> : null}
        {tab === "call" ? <CallPanel conv="demo-conv" ports={calls} /> : null}
      </main>
      {tab === "moments" ? (
        <StickerPanel ports={content} onPick={(sticker) => console.log("pick", sticker)} />
      ) : null}
    </div>
  );
}

createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <Harness />
  </StrictMode>,
);
