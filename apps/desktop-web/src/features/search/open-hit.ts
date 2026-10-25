import type { SearchHit } from "../../shell/models";
import { navigate } from "../../shell/router";

export function openHit(hit: SearchHit): void {
  if (hit.kind === "chat" && hit.conv_id) navigate({ id: "chat", convId: hit.conv_id });
  else if (hit.kind === "contact" && hit.user_id) navigate({ id: "contact", userId: hit.user_id });
  else if (hit.kind === "group" && hit.group_id) {
    if (hit.conv_id) navigate({ id: "chat", convId: hit.conv_id });
    else navigate({ id: "group", groupId: hit.group_id });
  } else navigate({ id: "search", query: hit.title });
}
