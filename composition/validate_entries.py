#!/usr/bin/env python3
"""G00-01 入口总账校验器（composition/validate_entries.py）。

校验 composition/entries.json：
  1. entry_id 唯一；
  2. 必填字段与枚举合法（scope/action_kind/handler_class/access/review_status）；
  3. 产品入口不得携带占位符（TO_EXPAND / TO_DEFINE_CONTRACT）；
  4. 产品入口的 owners.domain 与 test_id 必须已分配（无未分配项）；
  5. 每个 entry 的 capability_class 必须在 composition/platform-matrix.json 的
     classes 中登记；unsupported 组合必须有 alternative_path；
  6. counts.total 与 entries 实际数量一致（防手改漂移）；
  7. contract_candidate 必须携带完整状态 checklist 与非空 action_id 提案。

退出码：0 = 通过；1 = 存在违例（逐条打印）。
纯标准库，零依赖，供 CI 与本地复核使用（CI 接线见 integration-requests/G00/G00-01.yaml）。
"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LEDGER = ROOT / "composition/entries.json"
MATRIX = ROOT / "composition/platform-matrix.json"

PLACEHOLDERS = ("TO_EXPAND", "TO_DEFINE_CONTRACT", "HEURISTIC_REQUIRES_REVIEW")
ENUMS = {
    "scope": {"product", "showcase"},
    "platform_design": {"desktop_design", "mobile_design", "showcase"},
    "action_kind": {"local_ui", "contract_candidate", "planned"},
    "handler_class": {"ui.state", "ui.scroll", "ui.select", "nav.goto",
                      "implemented", "no_handler"},
    "review_status": {"owner_confirmed", "requires_domain_review",
                      "requires_ui_review"},
    "access": {"local", "read", "write"},
}
FULL_STATES = ("success", "empty_result", "param_error", "unauthorized",
               "timeout_or_offline", "cancel", "write_idempotency_required")


def main() -> int:
    errors: list[str] = []
    ledger = json.loads(LEDGER.read_text(encoding="utf-8"))
    matrix = json.loads(MATRIX.read_text(encoding="utf-8"))
    entries = ledger.get("entries", [])

    # 1) 唯一性
    seen: dict[str, int] = {}
    for i, e in enumerate(entries):
        eid = e.get("entry_id", "<missing>")
        if eid in seen:
            errors.append(f"[{i}] entry_id 重复: {eid}（首次出现于 #{seen[eid]}）")
        seen[eid] = i

    for i, e in enumerate(entries):
        eid = e.get("entry_id", f"<index {i}>")
        # 2) 必填字段与枚举
        for f in ("entry_id", "scope", "platform_design", "page_id", "component",
                  "interaction_kind", "source", "handler_class", "action_kind",
                  "capabilities", "states", "test_id", "owners", "review_status"):
            if f not in e:
                errors.append(f"[{eid}] 缺少必填字段 {f}")
        for f, allowed in ENUMS.items():
            if f in e and e[f] not in allowed:
                errors.append(f"[{eid}] {f} 非法值: {e[f]!r}")
        acc = e.get("capabilities", {}).get("access")
        if acc not in ENUMS["access"]:
            errors.append(f"[{eid}] capabilities.access 非法值: {acc!r}")

        # 3) 占位符
        blob = json.dumps(e, ensure_ascii=False)
        for ph in PLACEHOLDERS:
            if ph in blob:
                errors.append(f"[{eid}] 携带占位符 {ph}（产品总账不得保留种子占位）")

        # 4) 产品入口分配
        if e.get("scope") == "product":
            dom = e.get("owners", {}).get("domain")
            if not dom or not str(dom).startswith("G"):
                errors.append(f"[{eid}] 产品入口未分配 domain owner")
            if not e.get("test_id", "").startswith("T-"):
                errors.append(f"[{eid}] 产品入口缺少 test_id")

        # 5) 能力矩阵覆盖
        ccls = e.get("capabilities", {}).get("capability_class")
        if ccls not in matrix.get("classes", {}):
            errors.append(f"[{eid}] capability_class 未在 platform-matrix 登记: {ccls!r}")
        else:
            for plat, spec in matrix["classes"][ccls].items():
                if not spec.get("supported") and not spec.get("alternative_path"):
                    errors.append(
                        f"[{eid}] 矩阵项 {ccls}@{plat} unsupported 但无 alternative_path")

        # 6) contract_candidate 完整性
        if e.get("action_kind") == "contract_candidate":
            if not e.get("action_id", "").startswith("contract."):
                errors.append(f"[{eid}] contract_candidate 缺少 contract.* action 提案")
            st = e.get("states", {})
            missing = [s for s in FULL_STATES if not st.get(s)]
            if missing:
                errors.append(f"[{eid}] contract_candidate 状态清单缺 {missing}")
            if not st.get("write_idempotency_required") and \
                    e.get("capabilities", {}).get("access") == "write":
                errors.append(f"[{eid}] 写动作必须要求幂等（write_idempotency_required）")

    # 6) counts 一致性
    counts = ledger.get("counts", {})
    if counts.get("total") != len(entries):
        errors.append(f"counts.total={counts.get('total')} 与 entries 数量 {len(entries)} 不一致")
    recomputed_kinds = {}
    for e in entries:
        recomputed_kinds[e["action_kind"]] = recomputed_kinds.get(e["action_kind"], 0) + 1
    if counts.get("by_action_kind") != recomputed_kinds:
        errors.append("counts.by_action_kind 与 entries 实际分布不一致")

    if errors:
        print(f"校验失败：{len(errors)} 项")
        for e in errors:
            print("  ✗", e)
        return 1
    print(f"校验通过：{len(entries)} 条入口；id 唯一；占位符 0；矩阵全覆盖；"
          f"counts 一致；contract_candidate 状态清单完整。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
