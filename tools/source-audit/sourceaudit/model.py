"""门禁结论的统一表示：一条 finding = 一个可追责的问题。

设计要点：**finding 必须能落到人和编号**。安全报告里最常见的废话是
"存在风险，建议关注"——没有 owner、没有到期日、没有 issue，就等于没人管。
所以 `owner` / `issue` / `expires` 是 finding 的一等字段，缺失时报告会显式
标成 `?`，而不是留空让人以为已处理。
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Iterable


class Severity(str, Enum):
    CRITICAL = "critical"
    HIGH = "high"
    MEDIUM = "medium"
    LOW = "low"
    INFO = "info"

    @property
    def rank(self) -> int:
        return {"critical": 4, "high": 3, "medium": 2, "low": 1, "info": 0}[self.value]

    @classmethod
    def parse(cls, value: str | None) -> "Severity":
        try:
            return cls((value or "").strip().lower())
        except ValueError:
            return cls.INFO


class Status(str, Enum):
    PASS = "pass"
    WARN = "warn"
    FAIL = "fail"
    EXEMPT = "exempt"


@dataclass
class Finding:
    """一条结论。`exempt=True` 表示"知情接受"，必须同时有 owner+issue+到期日。"""

    code: str
    status: Status
    severity: Severity
    subject: str
    message: str
    owner: str | None = None
    issue: str | None = None
    expires: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "code": self.code,
            "status": self.status.value,
            "severity": self.severity.value,
            "subject": self.subject,
            "message": self.message,
            "owner": self.owner,
            "issue": self.issue,
            "expires": self.expires,
        }

    def one_line(self) -> str:
        who = f"{self.owner or '?'} / {self.issue or '?'}"
        tail = f" [{self.status.value}]" if self.status is not Status.FAIL else ""
        extra = f"（例外至 {self.expires}）" if self.expires else ""
        return (
            f"{self.severity.value.upper():8} {self.code:14} {self.subject:44} "
            f"{self.message} ({who}){extra}{tail}"
        )


@dataclass
class Report:
    gate: str
    findings: list[Finding] = field(default_factory=list)
    meta: dict[str, Any] = field(default_factory=dict)

    # ---- 汇总 -------------------------------------------------------------
    def add(self, finding: Finding) -> None:
        self.findings.append(finding)

    def fail(
        self,
        code: str,
        subject: str,
        message: str,
        severity: Severity = Severity.HIGH,
        **kw: Any,
    ) -> None:
        self.add(Finding(code, Status.FAIL, severity, subject, message, **kw))

    def warn(self, code: str, subject: str, message: str, severity: Severity = Severity.MEDIUM, **kw: Any) -> None:
        self.add(Finding(code, Status.WARN, severity, subject, message, **kw))

    def ok(self, code: str, subject: str, message: str = "ok") -> None:
        self.add(Finding(code, Status.PASS, Severity.INFO, subject, message))

    def exempt(self, code: str, subject: str, message: str, owner: str, issue: str, expires: str) -> None:
        self.add(Finding(code, Status.EXEMPT, Severity.MEDIUM, subject, message, owner, issue, expires))

    def of(self, status: Status) -> list[Finding]:
        return [f for f in self.findings if f.status is status]

    @property
    def blocking(self) -> list[Finding]:
        """阻断项：FAIL 一律阻断；WARN 只在 --strict 下阻断（由 CLI 决定）。"""
        return self.of(Status.FAIL)

    def strict_blocking(self) -> list[Finding]:
        return [f for f in self.findings if f.status in (Status.FAIL, Status.WARN)]

    def counts(self) -> dict[str, int]:
        out = {s.value: 0 for s in Status}
        for f in self.findings:
            out[f.status.value] += 1
        return out

    # ---- 输出 -------------------------------------------------------------
    def render_text(self, limit: int = 40) -> str:
        lines = [f"== {self.gate} =="]
        interesting = [f for f in self.findings if f.status is not Status.PASS]
        exempt = [f for f in self.findings if f.status is Status.EXEMPT]
        for f in interesting:
            lines.append(f.one_line())
        shown = len(interesting)
        if shown == 0:
            lines.append("无问题项。")
        elif shown > limit:
            lines.append(f"... 另有 {shown - limit} 条未打印（用 --json 取全量）")
        counts = self.counts()
        verdict = "BLOCK" if self.blocking else "PASS"
        lines.append(
            f"-- {verdict}: fail={counts['fail']} warn={counts['warn']} "
            f"exempt={counts['exempt']} pass={counts['pass']}"
        )
        if self.meta:
            lines.append("-- meta: " + json.dumps(self.meta, ensure_ascii=False, sort_keys=True))
        _ = exempt
        return "\n".join(lines)

    def to_json(self, ok: bool) -> str:
        payload = {
            "gate": self.gate,
            "ok": ok,
            "counts": self.counts(),
            "meta": self.meta,
            "findings": [f.to_dict() for f in self.findings],
        }
        return json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def summarize(findings: Iterable[Finding]) -> str:
    counts: dict[str, int] = {}
    for f in findings:
        counts[f.status.value] = counts.get(f.status.value, 0) + 1
    return ", ".join(f"{k}={v}" for k, v in sorted(counts.items()))
