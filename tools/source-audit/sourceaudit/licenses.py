"""许可判定：把 SPDX 表达式映射成 allow / review / deny / unknown。

为什么不用现成的 license 库：判定规则本身是**项目政策**，不是通用算法。
本仓库是 AGPL-3.0-only，所以"AGPL 兼容"这件事只有我们能定义；把规则写进
`third_party/reviews/license-policy.json`，代码只负责忠实执行，评审时改 JSON
即可，不用改代码——这是"政策与实现分离"的最小形态。
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .util import DataError, load_json

# SPDX 表达式里的运算符；`WITH` 例外按整体原子处理（见 `_atoms`）
_OPERATORS = re.compile(r"\s+(OR|AND)\s+", re.IGNORECASE)


class Verdict:
    ALLOW = "allow"
    REVIEW = "review"
    DENY = "deny"
    UNKNOWN = "unknown"


@dataclass(frozen=True)
class Decision:
    verdict: str
    matched: str
    reason: str

    @property
    def can_vendor(self) -> bool:
        """能不能把这份代码复制进本仓库。`review` 不算能。"""
        return self.verdict == Verdict.ALLOW


class LicensePolicy:
    def __init__(self, data: dict[str, Any]) -> None:
        if not isinstance(data, dict):
            raise DataError("license-policy.json 顶层必须是对象")
        self.project_license: str = data.get("project_license", "")
        self._allow = {_key(e["id"]): e for e in data.get("allow", [])}
        self._review = {_key(e["id"]): e for e in data.get("review", [])}
        self._deny = {_key(e["id"]): e for e in data.get("deny", [])}
        if not self._allow:
            raise DataError("license-policy.json 的 allow 为空：任何许可都会被判 unknown")

    @classmethod
    def load(cls, path: Path) -> "LicensePolicy":
        return cls(load_json(path))

    # ---- 判定 -------------------------------------------------------------
    def decide(self, expression: str | None) -> Decision:
        text = (expression or "").strip()
        if not text or text.upper() in {"NOASSERTION", "NONE", "UNKNOWN"}:
            return Decision(Verdict.UNKNOWN, text or "<empty>", "未声明许可：不得 vendor")
        atoms, joiner = _atoms(text)
        decisions = [self._decide_atom(a) for a in atoms]
        if joiner == "AND":
            # 全部都要（AND 表达式任一 deny 即 deny，任一 unknown 即 unknown）
            for verdict in (Verdict.DENY, Verdict.UNKNOWN):
                hit = next((d for d in decisions if d.verdict == verdict), None)
                if hit:
                    return Decision(hit.verdict, hit.matched, f"AND 表达式中 {hit.matched}：{hit.reason}")
            review = next((d for d in decisions if d.verdict == Verdict.REVIEW), None)
            return review or decisions[0]
        # OR：任一项 allow 即可（使用者可选），否则取最坏
        for want in (Verdict.ALLOW, Verdict.REVIEW, Verdict.UNKNOWN):
            hit = next((d for d in decisions if d.verdict == want), None)
            if hit:
                return hit
        return decisions[0]

    def _decide_atom(self, atom: str) -> Decision:
        key = _key(atom)
        if key in self._deny:
            return Decision(Verdict.DENY, atom, self._deny[key].get("note", "政策禁止"))
        if key in self._allow:
            return Decision(Verdict.ALLOW, atom, self._allow[key].get("note", "兼容 AGPL-3.0-only"))
        if key in self._review:
            return Decision(Verdict.REVIEW, atom, self._review[key].get("note", "需逐文件评审"))
        # `GPL-3.0` 这种省略 -only/-or-later 的写法：按 -only 再试一次，别默默放行
        for suffix in ("-only", "-or-later"):
            if _key(atom + suffix) in self._allow:
                return Decision(Verdict.ALLOW, atom + suffix, "按 -only 规范化后命中 allow")
        return Decision(Verdict.UNKNOWN, atom, "策略未登记该许可：按 unknown 处理")


def _key(expr: str) -> str:
    return " ".join(expr.strip().split()).lower()


def _atoms(expression: str) -> tuple[list[str], str]:
    """拆 SPDX 表达式。只有单一运算符类型时按它合并（混合表达式极罕见）。"""
    parts = [p.strip() for p in _OPERATORS.split(expression) if p and p.strip()]
    if len(parts) == 1:
        return [expression.strip()], "OR"
    atoms = [p for p in parts if p.upper() not in {"OR", "AND"}]
    joiner = "AND" if re.search(r"\sAND\s", expression, re.IGNORECASE) else "OR"
    return atoms, joiner
