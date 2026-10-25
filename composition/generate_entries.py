#!/usr/bin/env python3
"""G00-01 入口总账生成器（确定性，可重跑）。

输入：
  docs/full-platform-delivery/entry_seed.csv   331 个 onClick 种子（交付方快照）
  design/ui-v2/prototype/src/                  UI v2 原型源码（非 click 交互证据）

输出：
  composition/entries.json          入口总账（种子展开 + 无handler增量 + 非 click 交互）
  composition/entries-summary.json  统计摘要（评审用）

规则（与 teams/G00.md G00-01 对应）：
  * handler_class 由 handler 表达式确定性分类，逐条可反查 source:line；
  * component -> (page, domain_area, domain_owner) 用显式映射表（本文件，评审对象）；
  * 业务动词命中 => action_kind=contract_candidate，给出待 G00-03 批准的 action 提案；
  * 右键/长按/回车/拖拽/系统返回：原型已实现的按证据行登记，未实现的登记为
    no_handler 计划位（interaction_kind 标注），不得静默丢失；
  * 种子数 331 != 功能总数：每个种子一条 entry，另有增量条目单独标注来源。
"""
import csv
import json
import hashlib
import re
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SEED = ROOT / "docs/full-platform-delivery/entry_seed.csv"
OUT_LEDGER = ROOT / "composition/entries.json"
OUT_SUMMARY = ROOT / "composition/entries-summary.json"

# component -> (page_id, domain_area, domain_owner)
# page_id 对应原型 types.ts 的 MOBILE_PAGES / DESKTOP_PAGES 注册表；
# domain_owner 对应 18 职责组（G02 关系 / G03 消息 / G04 文件 / G05 内容社交 /
# G06 账本 / G08 容器 / G09 通话 / G00 架构自身；ui owner 保持种子的 G10-G13）。
COMPONENT_MAP = {
    # showcase 演示外壳
    "App": ("showcase", "shell", "G00"),
    "DemoSwitch": ("showcase", "shell", "G00"),
    "DeviceToggle": ("showcase", "shell", "G00"),
    "PagePicker": ("showcase", "shell", "G00"),
    "ScrollLink": ("showcase", "shell", "G00"),
    # 桌面壳与桌面页
    "DesktopWeChat": ("chats", "shell", "G00"),
    "DesktopLoginPage": ("login", "account", "G02"),
    "ChatView": ("chat", "chat", "G03"),
    "MessageContent": ("chat", "chat", "G03"),
    "ChatListItem": ("chats", "chat", "G03"),
    "ContactsPage": ("contacts", "relations", "G02"),
    "FavoritesPage": ("favorites", "content", "G05"),
    "FilesPage": ("files", "storage", "G04"),
    "MomentsPage": ("moments", "social", "G05"),
    "ChannelsPage": ("channels", "social", "G05"),
    "MiniProgramsPage": ("mini", "container", "G08"),
    "WalletPage": ("wallet", "ledger", "G06"),
    "SettingsPage": ("settings", "account", "G02"),
    "PageHeader": ("shared", "shell", "G00"),
    "HeroAction": ("shared", "shell", "G00"),
    "FeedAvatarRow": ("moments", "social", "G05"),
    # 手机页
    "ChatsScreen": ("chats", "chat", "G03"),
    "ChatScreen": ("chat", "chat", "G03"),
    "ChatRow": ("chats", "chat", "G03"),
    "ContactsScreen": ("contacts", "relations", "G02"),
    "ProfileScreen": ("profile", "relations", "G02"),
    "NewFriendsScreen": ("newfriends", "relations", "G02"),
    "OfficialScreen": ("official", "relations", "G02"),
    "DiscoverScreen": ("discover", "discover", "G05"),
    "DiscoverRow": ("discover", "discover", "G05"),
    "MomentsScreen": ("moments", "social", "G05"),
    "ChannelsScreen": ("channels", "social", "G05"),
    "MiniProgramsScreen": ("mini", "container", "G08"),
    "SearchScreen": ("search", "discover", "G05"),
    "ScanScreen": ("scan", "discover", "G05"),
    "ReadingScreen": ("reading", "content", "G05"),
    "LocationScreen": ("location", "discover", "G05"),
    "MeScreen": ("me", "account", "G02"),
    "StatusScreen": ("status", "social", "G05"),
    "MyCodeScreen": ("mycode", "account", "G02"),
    "FavoritesScreen": ("favorites", "content", "G05"),
    "CardsScreen": ("cards", "ledger", "G06"),
    "StickersScreen": ("stickers", "content", "G05"),
    "SettingsScreen": ("settings", "account", "G02"),
    "LoginScreen": ("login", "account", "G02"),
    "WalletScreen": ("wallet", "ledger", "G06"),
    "PayScreen": ("pay", "ledger", "G06"),
    "TransferScreen": ("transfer", "ledger", "G06"),
    "RedPacketScreen": ("redpacket", "ledger", "G06"),
    "CallScreen": ("call", "rtc", "G09"),
    # 共享原子
    "MobileCell": ("shared", "shell", "G00"),
    "MobileTop": ("shared", "shell", "G00"),
    "PhoneBack": ("shared", "nav", "G00"),
    "TabBar": ("shared", "nav", "G00"),
    "SearchBar": ("shared", "search", "G05"),
    "SearchPill": ("shared", "search", "G05"),
    "Switch": ("shared", "shell", "G00"),
}

WRITE_VERBS = {
    "send", "pay", "transfer", "publish", "post", "add", "delete", "upload",
    "create", "invite", "recall", "topup", "recharge", "withdraw", "submit",
    "confirm", "block", "mute", "kick", "rename",
}
BUSINESS_VERBS = re.compile(
    r"send|pay|transfer|redpacket|add|publish|post|like|comment|delete|login|scan"
    r"|topup|recharge|withdraw|favorite|download|upload|call|join|create|rename"
    r"|invite|kick|block|mute|recall|accept|decline|submit|confirm", re.I)


def classify_handler(handler: str):
    """确定性分类 handler 表达式 -> (handler_class, action_kind, action_id, verb)。"""
    h = handler.strip()
    if "scrollIntoView" in h:
        return "ui.scroll", "local_ui", "proto.ui.scroll", None
    if re.search(r"navigate\(|setPage|setScreen|setMobileScreen|setDesktopPage", h):
        return "nav.goto", "local_ui", "proto.nav.goto", None
    if h.startswith("onChange") or h.startswith("onSelect") or "onChange(" in h:
        return "ui.select", "local_ui", "proto.ui.select", None
    m = BUSINESS_VERBS.search(h)
    if m:
        return "ui.state", "contract_candidate", None, m.group(0).lower()
    return "ui.state", "local_ui", "proto.ui.state", None


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def states_for(akind: str) -> dict:
    """卡面要求的状态清单：业务动作全 checklist；本地视觉态记相关子集。"""
    if akind == "contract_candidate":
        return {
            "success": True, "empty_result": True, "param_error": True,
            "unauthorized": True, "timeout_or_offline": True, "cancel": True,
            "write_idempotency_required": True,
        }
    return {
        "success": True, "empty_result": False, "param_error": False,
        "unauthorized": False, "timeout_or_offline": False, "cancel": True,
        "write_idempotency_required": False,
        "note": "prototype local ui state; backend checklist applies when promoted",
    }


def collect_seed_entries() -> list:
    out = []
    for r in csv.DictReader(open(SEED, encoding="utf-8")):
        comp = r["component"]
        page, area, dom = COMPONENT_MAP.get(comp, ("shared", "shell", "G00"))
        hcls, akind, aid, verb = classify_handler(r["handler"])
        scope = "showcase" if r["platform"] == "showcase" else "product"
        if akind == "contract_candidate" and verb:
            aid = f"contract.{area}.{verb}"
        access = "local"
        if akind == "contract_candidate":
            access = "write" if verb in WRITE_VERBS else "read"
        out.append({
            "entry_id": f"ENT-{r['entry_id']}",
            "seed_id": r["entry_id"],
            "scope": scope,
            "platform_design": r["platform"],
            "page_id": page,
            "component": comp,
            "interaction_kind": "click",
            "source": {
                "file": f"design/ui-v2/prototype/src/{r['source']}",
                "line": int(r["line"]),
                "column": int(r["column"]),
            },
            "handler_class": hcls,
            "action_id": aid,
            "action_kind": akind,
            "capabilities": {"access": access, "capability_class": hcls},
            "states": states_for(akind),
            "test_id": f"T-{r['entry_id']}",
            "owners": {"domain": dom, "ui": r["ui_owner"], "domain_area": area},
            "provenance": {"seed_sha256": r["source_sha256"]},
            "review_status": ("owner_confirmed" if akind == "local_ui"
                              else "requires_domain_review"),
        })
    return out


def collect_non_click() -> list:
    """非 click 交互：原型已实现（证据行）与必须新增的计划位（no_handler）。"""
    entries = []

    def add(eid, page, comp, kind, status, file, line, action, ui, note):
        page_, area, dom = COMPONENT_MAP.get(comp, ("shared", "shell", "G00"))
        entries.append({
            "entry_id": eid,
            "seed_id": None,
            "scope": "showcase" if page == "showcase" else "product",
            "platform_design": ("desktop_design" if "desktop" in str(file)
                                else "mobile_design"),
            "page_id": page_,
            "component": comp,
            "interaction_kind": kind,
            "source": {"file": file, "line": line, "column": None},
            "handler_class": ("implemented" if status == "implemented"
                              else "no_handler"),
            "action_id": action,
            "action_kind": ("local_ui" if status == "implemented" else "planned"),
            "capabilities": {"access": "local", "capability_class": kind},
            "states": states_for("local_ui"),
            "test_id": f"T-{eid}",
            "owners": {"domain": dom, "ui": ui, "domain_area": area},
            "provenance": {"seed_sha256": None},
            "review_status": "requires_ui_review",
            "notes": note,
        })

    # 已实现（证据行核对过）：Enter 发送 / Shift+Enter 换行
    add("ENT-NH-ENTER-SEND", "chat", "ChatView", "keyboard.enter", "implemented",
        "design/ui-v2/prototype/src/components/desktop/ChatView.tsx", 342,
        "proto.input.send", "G10",
        "Enter 发送 / Shift+Enter 换行已实现；产品化时挂 message.send 契约")
    # 计划位（issue #99 UI v2 增量要求 + 卡面"右键/长按/拖拽/系统返回"）
    planned = [
        ("ENT-NH-CONTEXT-MENU", "chat", "ChatView", "context.menu",
         "桌面消息右键菜单（引用/复制/撤回/删除）"),
        ("ENT-NH-LONG-PRESS", "chats", "ChatsScreen", "long.press",
         "移动端会话长按菜单（置顶/免打扰/删除）"),
        ("ENT-NH-SYSTEM-BACK", "shared", "PhoneBack", "system.back",
         "安卓系统返回/手势返回的页面栈语义"),
        ("ENT-NH-DRAG-MOMENTS", "moments", "MomentsScreen", "drag",
         "朋友圈下拉刷新与图片拖拽预览"),
        ("ENT-NH-COUNTRY-CODE", "login", "LoginScreen", "click",
         "无 handler：登录页国家码选择器"),
        ("ENT-NH-PRIVACY-LINKS", "login", "LoginScreen", "click",
         "无 handler：协议与隐私链接"),
        ("ENT-NH-PLACE-SEARCH", "location", "LocationScreen", "click",
         "无 handler：位置页搜索地点"),
        ("ENT-NH-NAV-ICONS", "shared", "TabBar", "click",
         "无 handler：导航图标位（发现/我等）"),
        ("ENT-NH-STATUS-SEARCH", "status", "StatusScreen", "click",
         "无 handler：状态页搜索"),
        ("ENT-NH-TRANSFER-MORE", "transfer", "TransferScreen", "click",
         "无 handler：转账更多/支付方式"),
        ("ENT-NH-OFFICIAL-SEARCH-MORE", "official", "OfficialScreen", "click",
         "无 handler：订阅号搜索更多"),
        ("ENT-NH-MYCODE-MORE", "mycode", "MyCodeScreen", "click",
         "无 handler：个人码更多"),
        ("ENT-NH-SHOWCASE-DEVICE", "showcase", "DeviceToggle", "click",
         "无 handler：showcase 设备选择器（App.tsx:136-138 双端/桌面/手机）"),
        ("ENT-NH-SHOWCASE-PAGES", "showcase", "PagePicker", "click",
         "无 handler：showcase 页面选择器全量枚举"),
    ]
    for eid, page, comp, kind, note in planned:
        add(eid, page, comp, kind, "no_handler", "issue:99#ui-v2增量要求", 0,
            None, "G11", note)
    return entries


def main() -> None:
    seeds = collect_seed_entries()
    extras = collect_non_click()
    entries = seeds + extras
    ledger = {
        "contract_version": "entries-v1",
        "generated_by": "composition/generate_entries.py",
        "inputs": {
            "entry_seed_csv": "docs/full-platform-delivery/entry_seed.csv",
            "entry_seed_sha256": sha256(SEED),
            "prototype_src": "design/ui-v2/prototype/src",
        },
        "counts": {
            "seeds": len(seeds),
            "non_seed_expansions": len(extras),
            "total": len(entries),
            "by_action_kind": dict(Counter(e["action_kind"] for e in entries)),
            "by_domain": dict(Counter(e["owners"]["domain"] for e in entries)),
        },
        "entries": entries,
    }
    OUT_LEDGER.write_text(json.dumps(ledger, ensure_ascii=False, indent=1),
                          encoding="utf-8")
    summary = {
        "total": len(entries),
        "by_action_kind": ledger["counts"]["by_action_kind"],
        "by_domain": ledger["counts"]["by_domain"],
        "by_scope": dict(Counter(e["scope"] for e in entries)),
        "by_platform_design": dict(Counter(e["platform_design"] for e in entries)),
        "pages_registered": sorted({e["page_id"] for e in entries}),
    }
    OUT_SUMMARY.write_text(json.dumps(summary, ensure_ascii=False, indent=1),
                           encoding="utf-8")
    print(json.dumps(summary, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
