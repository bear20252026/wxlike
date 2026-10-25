/**
 * VideoPlayer (G11-03): actual progressive playback of the real mp4 artifact
 * (`/api/video/{id}/play.mp4` — not a static cover), paused AND released
 * whenever the page leaves sight. Release means: pause, drop `src`, `load()`
 * — network and decoder are freed, so background tabs stay resource-bounded.
 */

import { useEffect, useRef, useState } from "react";
import type { PlaybackUrls, VideoMeta } from "./model";
import { usePlaybackGate } from "./gate";

export function VideoPlayer({
  videoId,
  urls,
  meta,
  onViewed,
}: {
  videoId: string;
  urls: PlaybackUrls;
  meta?: VideoMeta;
  onViewed?: () => void;
}) {
  const boxRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const visible = usePlaybackGate(boxRef);
  const [failed, setFailed] = useState(false);
  const viewedRef = useRef(false);

  // Release when invisible; attach when visible again. Keep the element
  // mounted (position must not jump) but strip the media resource.
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    if (visible) {
      if (!video.getAttribute("src")) {
        video.src = urls.mp4;
        video.load();
      }
    } else {
      video.pause();
      video.removeAttribute("src");
      video.load();
    }
  }, [visible, urls.mp4]);

  const onPlay = () => {
    if (!viewedRef.current) {
      viewedRef.current = true;
      onViewed?.();
    }
  };

  return (
    <div ref={boxRef} className="overflow-hidden rounded-xl bg-black" data-testid={`player-${videoId}`}>
      {failed ? (
        <div className="flex aspect-video items-center justify-center text-sm text-zinc-400">
          视频加载失败
        </div>
      ) : (
        <video
          ref={videoRef}
          controls
          playsInline
          preload="none"
          poster={visible ? urls.thumb : undefined}
          width={meta?.width}
          height={meta?.height}
          data-testid={`video-${videoId}`}
          onError={() => setFailed(true)}
          onPlay={onPlay}
          className="aspect-video w-full"
        />
      )}
    </div>
  );
}
