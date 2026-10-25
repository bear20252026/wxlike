"""wxlike 源码审计工具包（G17）。

只依赖 Python 标准库：CI 的 quality job 不装第三方包，门禁工具自己也不能是
一个需要维护依赖树的东西。
"""

__all__ = ["advisories", "authz", "cli", "licenses", "model", "notice", "provenance", "release", "sbom", "util"]
__version__ = "1.0"
