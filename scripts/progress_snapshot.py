#!/usr/bin/env python3
"""单一权威进度源（第四轮审计 N5）：把四源漂移收敛成一份带核对日期的机器可读快照。

为什么需要它：想知道"项目到底完成多少"，以前要同时读 tasks.json（全 PLANNED，
误导）、GitHub issue 开闭、各组 delivery.md 自述、蓝图（过时）四个互相矛盾的源，
且没有一个是权威。本脚本以**代码事实**为准生成一份快照，GitHub 开闭只作旁证
（issue 闭 ≠ 验收过），蓝图/delivery 自述不作完成度依据。

完成度分档（与第四轮审计 §4 一致，逐卡判定）：
  LANDED   代码+接线+测试存在（不代表真机/生产验证）
  SEAM     域实现就绪、未接生产（孤岛台账登记）
  PARTIAL  部分接线 / 域侧空 / 上游缺口
  MISSING  未建
  RETIRED  已退役（ADR 取代）

证据源（全部仓库内、可复现，不依赖实时网络）：
  - docs/full-platform-delivery/tasks.json     卡全集骨架（101 卡）
  - docs/full-platform-delivery/teams/Gxx.md   卡标题双射核对
  - scripts/wiring-baseline.json               孤岛 SEAM 台账（crate→owner/task）
  - ci/jobs/policy/known-gaps.json             缺口与 blocks_release
  - integration-requests/Gxx/*.yaml            跨组请求与 ready-for-G00 积压
  - issues-state.json（可选，--issues）         GitHub 开闭旁证，非完成度依据

用法：
    python scripts/progress_snapshot.py                 # 生成快照到 stdout
    python scripts/progress_snapshot.py --write         # 写入 docs/full-platform-delivery/progress-snapshot.json
    python scripts/progress_snapshot.py --check         # 校验已写快照与当前事实一致（CI 用），漂移则非零
    python scripts/progress_snapshot.py --issues FILE   # 纳入 GitHub 开闭旁证
退出码：0 干净 / 1 漂移或违规 / 2 跑不起来。
"""

from __future__ import annotations

import argparse
import datetime
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
TASKS = ROOT / "docs/full-platform-delivery/tasks.json"
TEAMS_DIR = ROOT / "docs/full-platform-delivery/teams"
WIRING = ROOT / "scripts/wiring-baseline.json"
GAPS = ROOT / "ci/jobs/policy/known-gaps.json"
IR_DIR = ROOT / "integration-requests"
SNAPSHOT = ROOT / "docs/full-platform-delivery/progress-snapshot.json"

# 逐组完成度判定（第四轮审计 §4 的代码事实结论）。这是**审计员对账后的判定**，
# 不是从 tasks.json 的 status 推出来的（那 100 卡全 PLANNED，正是 N5 要治的误导）。
# 每组给 default 档 + 例外卡 + 证据/缺口说明；新增卡未登记时落 default 并在
# --check 里提示需要人工复核，不许静默当成已完成。
GROUP_ASSESSMENT = {
    "G00": {"default": "PARTIAL", "evidence": "拆分/契约/所有权落地；PG 迁移未做（G00-04）；G06/G14 集成请求积压 ready-for-G00", "overrides": {}},
    "G01": {"default": "LANDED", "evidence": "类型/核心/UniFFI/生成落地", "overrides": {"G01-03": ("SEAM", "client-web WASM 入口未打包进 desktop-web")}},
    "G02": {"default": "PARTIAL", "evidence": "会话/关系/拉黑/群管理落地；G02-09 治理域已实现（封禁/解封/撤设备/群治理+元数据视图，38 测试）；E2EE 库就绪新 UI 未接", "overrides": {"G02-09": ("PARTIAL", "2026-11-19 域侧实现（identity/relations governance）；HTTP 面挂载/平台会话联动/users 表迁移编号待 G00 集成（G02-09-governance.yaml ready-for-G00）")}},
    "G03": {"default": "LANDED", "evidence": "读扩散信箱/多设备/WS/未读落地；推送聚合已实现，APNs 未接", "overrides": {}},
    "G04": {"default": "LANDED", "evidence": "media_mount 装配、备份导入回滚落地", "overrides": {}},
    "G05": {"default": "PARTIAL", "evidence": "朋友圈/收藏/贴纸/搜索落地；内容治理 admin 端点不存在（G14-03 上游缺口）", "overrides": {}},
    "G06": {"default": "PARTIAL", "evidence": "commerce+ledger 域实现+测试完整；回调+全部动作面已挂载（2026-11-19：通用动作面/旧 webhook 入口/audience 闸门/真实群成员端口/到期与对账后台作业），PAYMENT-CALLBACK-REPLAY 已关闭；七个集成请求 executed-pending-live-evidence，真实供应商取证归 G16、PG 迁移归 G00-04", "overrides": {}},
    "G07": {"default": "PARTIAL", "evidence": "service-catalog 已挂载；discovery（扫码/二维码/附近）2026-11-19 接线；vision/translation 编排面已挂载但无供应商凭据=诚实 503 拒绝面（真实适配器待接）；购物/游戏 provider 未接", "overrides": {}},
    "G08": {"default": "PARTIAL", "evidence": "container/dpage/miniapps 2026-11-19 经 mini_runtime 真实接线（审核目录搜索/推荐、包检视、根信任验签、容器沙箱启动）；安装/签包发布诚实 501（缺签名上传流式核心与离线根密钥）；SEAM 登记已移除", "overrides": {}},
    "G09": {"default": "PARTIAL", "evidence": "str0m 媒体+房间制+直播控制面+信令落地；ADR-25 门禁对齐但 LiveKit sidecar 未接", "overrides": {}},
    "G10": {"default": "LANDED", "evidence": "desktop-web 262 测试、核心装配/会话边界/历史分页落地；旧 UI 与 raw IPC 已退役（功能等价待补）", "overrides": {}},
    "G11": {"default": "LANDED", "evidence": "wallet/files/mini/discover/video/calls 功能页+core-only 迁移层落地", "overrides": {}},
    "G12": {"default": "RETIRED", "evidence": "Flutter 退役（ADR-29）；原生壳未建（MOBILE-NATIVE-SHELLS）", "overrides": {}},
    "G13": {"default": "RETIRED", "evidence": "Flutter 退役（ADR-29）；团队卡未标退役", "overrides": {}},
    "G14": {"default": "PARTIAL", "evidence": "RBAC/审计/幂等真实（d1/d2/d3 打真平台）；域侧动作全 fixture、上游端点不存在（目标已纠正 G02-09/G08-01/G07-04/G06-03）、未部署（N2/N3，缺口 ADMIN-NOT-DEPLOYED 已显式登记）", "overrides": {}},
    "G15": {"default": "LANDED", "evidence": "50 命令目录、MCP tools/list+call、AuthGate+Policy 落地", "overrides": {}},
    "G16": {"default": "PARTIAL", "evidence": "CI 门禁/发布证据链/桌面打包落地；发布评审新鲜度守卫已落地（release check 核 commit 距离/评审日期，N6），但证据包本身仍过时（落后 298 commit，待重新评审）；设备证据缺", "overrides": {}},
    "G17": {"default": "LANDED", "evidence": "威胁模型/越权/滥用/模糊测试 51 条、来源审查落地；评审证据过时（N6）", "overrides": {}},
}


def load_json(path):
    # utf-8-sig 兼容带 BOM 的文件（PowerShell `Out-File -Encoding utf8` 会加 BOM）。
    return json.loads(path.read_text(encoding="utf-8-sig"))


def team_card_ids():
    """teams/Gxx.md 里的卡 ID 全集（## Gxx-nn｜标题）。"""
    ids = set()
    for path in sorted(TEAMS_DIR.glob("G*.md")):
        for m in re.finditer(r"^##\s+(G\d{2}-\d{2})｜", path.read_text(encoding="utf-8"), re.M):
            ids.add(m.group(1))
    return ids


def seam_tasks():
    """孤岛台账里的 task 引用（可能形如 G06-03 或 G06）。"""
    data = load_json(WIRING)
    out = {}
    for seam in data.get("seams", []):
        out[seam.get("task", "")] = seam.get("crate", "")
    return out


def gap_index():
    data = load_json(GAPS)
    return {g["id"]: g for g in data.get("gaps", [])}


def ir_backlog():
    """统计 ready-for-G00 积压（按组）。"""
    counts = {}
    for path in sorted(IR_DIR.glob("G*/*.yaml")):
        group = path.parent.name
        try:
            import yaml
            doc = yaml.safe_load(path.read_text(encoding="utf-8"))
        except Exception:
            continue
        if isinstance(doc, dict) and doc.get("status") == "ready-for-G00":
            counts[group] = counts.get(group, 0) + 1
    return counts


def assess_card(card, seams):
    group = card["group"]
    cid = card["id"]
    ga = GROUP_ASSESSMENT.get(group)
    if ga is None:
        return "MISSING", f"组 {group} 未在审计判定中登记——需人工复核"
    if cid in ga["overrides"]:
        return ga["overrides"][cid]
    return ga["default"], ga["evidence"]


def build_snapshot(issues_path=None):
    tasks = load_json(TASKS)
    team_ids = team_card_ids()
    seams = seam_tasks()
    gaps = gap_index()
    backlog = ir_backlog()

    issues = {}
    if issues_path and pathlib.Path(issues_path).is_file():
        for it in load_json(pathlib.Path(issues_path)):
            issues[it["number"]] = it["state"]

    cards = []
    drift = []
    for card in tasks:
        cid = card["id"]
        # 双射核对：tasks.json 与 teams 卡必须一一对应。
        if cid not in team_ids:
            drift.append(f"{cid} 在 tasks.json 但 teams 卡缺失")
        status, evidence = assess_card(card, seams)
        issue_no = card.get("github_issue_number")
        issue_state = issues.get(issue_no) if issue_no else None
        cards.append({
            "id": cid,
            "group": card["group"],
            "title": card["title"],
            "wave": card.get("wave"),
            "status": status,
            "evidence": evidence,
            "tasks_json_status": card.get("status"),
            "github_issue_number": issue_no,
            "github_issue_state": issue_state,
        })
    for tid in sorted(team_ids - {c["id"] for c in tasks}):
        drift.append(f"{tid} 在 teams 卡但 tasks.json 缺失")

    by_status = {}
    by_group = {}
    for c in cards:
        by_status[c["status"]] = by_status.get(c["status"], 0) + 1
        g = by_group.setdefault(c["group"], {"total": 0, "statuses": {}})
        g["total"] += 1
        g["statuses"][c["status"]] = g["statuses"].get(c["status"], 0) + 1

    blocking = [gid for gid, g in gaps.items() if g.get("status") == "open" and g.get("blocks_release")]

    snapshot = {
        "schema": 1,
        "verified_at": datetime.date.today().isoformat(),
        "authority": "代码事实（孤岛台账/缺口台账/集成请求/审计判定）；GitHub 开闭仅旁证，issue 闭≠验收过",
        "note": "单一权威进度源（第四轮审计 N5）。替代 tasks.json/GitHub/delivery/蓝图四源手工对账。LANDED 指代码+接线+测试存在，不代表真机/生产验证。",
        "card_count": len(cards),
        "by_status": by_status,
        "by_group": by_group,
        "release_blocking_gaps": blocking,
        "ready_for_g00_backlog": backlog,
        "cards": cards,
    }
    return snapshot, drift


def main(argv=None):
    parser = argparse.ArgumentParser(description="单一权威进度源生成/校验")
    parser.add_argument("--write", action="store_true", help="写入 progress-snapshot.json")
    parser.add_argument("--check", action="store_true", help="校验已写快照与当前事实一致")
    parser.add_argument("--issues", default=None, help="GitHub issue 状态 JSON（旁证）")
    args = parser.parse_args(argv)

    if not TASKS.is_file():
        print(f"找不到任务表：{TASKS}", file=sys.stderr)
        return 2
    snapshot, drift = build_snapshot(args.issues)

    if args.check:
        if not SNAPSHOT.is_file():
            print(f"缺少已写快照：{SNAPSHOT}（先跑 --write）", file=sys.stderr)
            return 1
        committed = load_json(SNAPSHOT)
        problems = list(drift)
        # 逐卡状态对账：快照里的判定必须与当前事实一致。
        cur = {c["id"]: c["status"] for c in snapshot["cards"]}
        old = {c["id"]: c["status"] for c in committed.get("cards", [])}
        for cid in sorted(set(cur) | set(old)):
            if cur.get(cid) != old.get(cid):
                problems.append(f"{cid} 完成度漂移：快照 {old.get(cid)} → 当前 {cur.get(cid)}")
        if committed.get("card_count") != snapshot["card_count"]:
            problems.append(f"卡数漂移：快照 {committed.get('card_count')} → 当前 {snapshot['card_count']}")
        for line in problems:
            print(f"进度漂移：{line}", file=sys.stderr)
        print(f"进度快照校验：{snapshot['card_count']} 卡，漂移 {len(problems)} 条")
        return 1 if problems else 0

    text = json.dumps(snapshot, ensure_ascii=False, indent=2) + "\n"
    if args.write:
        SNAPSHOT.write_text(text, encoding="utf-8")
        print(f"已写入 {SNAPSHOT.relative_to(ROOT)}（{snapshot['card_count']} 卡，核对日期 {snapshot['verified_at']}）")
        for line in drift:
            print(f"双射警告：{line}", file=sys.stderr)
        return 1 if drift else 0
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())