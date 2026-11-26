"""Stop-write window measurement for the cutover run."""

from __future__ import annotations

import time
from dataclasses import dataclass, field


def now_ms() -> int:
    return int(time.time() * 1000)


@dataclass
class StopWriteWindow:
    source_sha256: str
    source_data_version: int
    started_at_ms: int = field(default_factory=now_ms)
    finished_at_ms: int | None = None
    after_sha256: str | None = None
    after_data_version: int | None = None
    table_ms: dict[str, int] = field(default_factory=dict)

    def record_table(self, table: str, elapsed_ms: int) -> None:
        self.table_ms[table] = elapsed_ms

    def close(self, after_sha256: str, after_data_version: int) -> None:
        self.finished_at_ms = now_ms()
        self.after_sha256 = after_sha256
        self.after_data_version = after_data_version

    @property
    def duration_ms(self) -> int:
        end = self.finished_at_ms if self.finished_at_ms is not None else now_ms()
        return max(0, end - self.started_at_ms)

    @property
    def quiescent(self) -> bool:
        return (
            self.after_sha256 == self.source_sha256
            and self.after_data_version == self.source_data_version
        )

    def to_dict(self) -> dict:
        return {
            "source_sha256": self.source_sha256,
            "source_data_version": self.source_data_version,
            "after_sha256": self.after_sha256,
            "after_data_version": self.after_data_version,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "duration_ms": self.duration_ms,
            "quiescent": self.quiescent,
            "table_ms": dict(sorted(self.table_ms.items())),
        }
