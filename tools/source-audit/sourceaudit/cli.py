"""命令行入口：`python -m sourceaudit <gate> <action> [选项]`。

退出码是契约：CI 只认数字。0 通过 · 2 用法/参数错 · 3 策略不通过 · 4 数据缺失
或网络不可达 · 130 被取消。把"工具跑不动"和"检查没通过"分开，才不会出现
"挂了三天，其实是路径写错"。
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Callable

from . import notice, provenance, sbom
from .model import Report, Severity
from .util import EXIT_BLOCK, EXIT_OK, AuditError, repo_root


def _common(parser: argparse.ArgumentParser, suppress: bool = False) -> None:
    """公共选项。叶子命令也要挂一份：`check --json` 比 `--json check` 自然。

    `suppress=True` 时不给默认值，避免子命令的默认值把顶层已解析的值冲掉。
    """
    d = argparse.SUPPRESS if suppress else None
    default_root = argparse.SUPPRESS if suppress else None
    parser.add_argument("--root", type=Path, default=default_root, help="仓库根（默认自动向上查找）")
    parser.add_argument("--json", action="store_true", default=(argparse.SUPPRESS if suppress else False), help="机器可读输出")
    parser.add_argument("--strict", action="store_true", default=(argparse.SUPPRESS if suppress else False), help="warn 也阻断")
    _ = d


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="sourceaudit",
        description="wxlike 来源/许可/漏洞/越权/发布门禁（G17）",
    )
    _common(parser)
    sub = parser.add_subparsers(dest="gate", required=True)

    prov = sub.add_parser("provenance", help="来源登记核对（G17-01）")
    _common(prov, suppress=True)
    prov_sub = prov.add_subparsers(dest="action", required=True)
    prov_check = prov_sub.add_parser("check", help="核对 registry 与本地文件")
    _common(prov_check, suppress=True)
    prov_check.add_argument("--registry", type=Path, default=None)

    sb = sub.add_parser("sbom", help="SBOM 生成与漂移检查（G17-01）")
    _common(sb, suppress=True)
    sb_sub = sb.add_subparsers(dest="action", required=True)
    sb_gen = sb_sub.add_parser("generate", help="从锁文件生成 CycloneDX")
    _common(sb_gen, suppress=True)
    sb_gen.add_argument("-o", "--out", type=Path, default=None)
    sb_check = sb_sub.add_parser("check", help="与已提交 SBOM 比对")
    _common(sb_check, suppress=True)

    nt = sub.add_parser("notice", help="NOTICE 覆盖检查（G17-01）")
    _common(nt, suppress=True)
    nt_sub = nt.add_subparsers(dest="action", required=True)
    nt_check = nt_sub.add_parser("check")
    _common(nt_check, suppress=True)
    nt_check.add_argument("--notice", type=Path, default=None)

    adm = sub.add_parser("advisories", help="漏洞与依赖准入（G17-02）")
    _common(adm, suppress=True)
    adm_sub = adm.add_subparsers(dest="action", required=True)
    adm_check = adm_sub.add_parser("check", help="按政策判定公告")
    _common(adm_check, suppress=True)
    adm_check.add_argument("--offline", action="store_true", help="只用本地缓存，不联网")
    adm_check.add_argument("--input", type=Path, default=None, help="用给定的公告 JSON 代替扫描结果")
    adm_check.add_argument("--timeout", type=float, default=10.0, help="联网超时秒数")

    az = sub.add_parser("authz", help="授权矩阵与越权用例核对（G17-03）")
    _common(az, suppress=True)
    az_sub = az.add_subparsers(dest="action", required=True)
    az_sync = az_sub.add_parser("sync", help="从源码路由重建矩阵（保留人工字段）")
    _common(az_sync, suppress=True)
    az_sync.add_argument("--write", action="store_true", help="写回矩阵文件")
    az_check = az_sub.add_parser("check", help="源码/矩阵/用例三方核对")
    _common(az_check, suppress=True)

    rel = sub.add_parser("release", help="发布评审证据判定（G17-05）")
    _common(rel, suppress=True)
    rel_sub = rel.add_subparsers(dest="action", required=True)
    rel_check = rel_sub.add_parser("check", help="证据包 → pass/block")
    _common(rel_check, suppress=True)
    rel_check.add_argument("--evidence", type=Path, default=None)

    every = sub.add_parser("all", help="跑全部已实现的门禁（CI 用）")
    _common(every, suppress=True)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        root = (args.root or repo_root()).resolve()
        if args.root and not (root / "Cargo.toml").is_file():
            # 显式给了 --root 但不是仓库：宁可报错，也不要在空目录上"全绿"
            from .util import UsageError

            raise UsageError(f"--root 不是仓库根（缺 Cargo.toml）：{root}")
        reports = _dispatch(args, root)
    except AuditError as exc:
        print(f"sourceaudit: {exc}", file=sys.stderr)
        return exc.exit_code
    except KeyboardInterrupt:
        # 取消就是取消：不写半截产物、不把中断当通过
        print("sourceaudit: cancelled", file=sys.stderr)
        return 130
    return _emit(args, reports)


def _dispatch(args: argparse.Namespace, root: Path) -> list[Report]:
    gate, action = args.gate, getattr(args, "action", None)
    if gate == "provenance":
        return [provenance.check(root, args.registry)]
    if gate == "sbom" and action == "generate":
        out = args.out or root / "third_party/reviews/sbom/wxlike.cdx.json"
        changed, count = sbom.write(root, out)
        report = Report("sbom-generate", meta={"out": str(out), "components": count, "changed": changed})
        report.ok("SBOM-WRITE", out.name, f"{count} 个组件（{'已更新' if changed else '未变化'}）")
        return [report]
    if gate == "sbom":
        return [sbom.check(root)]
    if gate == "notice":
        return [notice.check(root, args.notice)]
    if gate == "advisories":
        from . import advisories

        return [advisories.check(root, offline=args.offline, source=args.input, timeout=args.timeout)]
    if gate == "authz":
        from . import authz

        if action == "sync":
            return [authz.sync(root, write=args.write)]
        return [authz.check(root)]
    if gate == "release":
        from . import release

        return [release.check(root, evidence=args.evidence)]
    if gate == "all":
        return _all(root)
    raise AuditError(f"未知命令：{gate} {action}")


def _all(root: Path) -> list[Report]:
    from . import advisories, authz, release

    gates: list[Callable[[], Report]] = [
        lambda: provenance.check(root),
        lambda: sbom.check(root),
        lambda: notice.check(root),
        lambda: advisories.check(root, offline=False, source=None, timeout=10.0),
        lambda: authz.check(root),
        lambda: release.check(root, evidence=None),
    ]
    # 数据缺失/联网异常必须沿 main 的 AuditError 契约返回非零，不能降成 WARN。
    return [gate() for gate in gates]


def _emit(args: argparse.Namespace, reports: list[Report]) -> int:
    blocked = False
    for report in reports:
        hits = report.strict_blocking() if args.strict else report.blocking
        blocked = blocked or bool(hits)
        if args.json:
            sys.stdout.write(report.to_json(ok=not hits))
        else:
            print(report.render_text())
    if blocked:
        bad = [f for r in reports for f in (r.strict_blocking() if args.strict else r.blocking)]
        if not args.json:
            print(f"-- 阻断项 {len(bad)} 条；用 --json 取全量", file=sys.stderr)
        return EXIT_BLOCK
    return EXIT_OK
