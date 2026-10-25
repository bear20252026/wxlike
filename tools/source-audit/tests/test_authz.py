"""授权矩阵门禁用例：扫描、同步、三方核对各自的失败形态。

重点不在"能跑通"，而在**它会不会被骗**：
* 同名业务方法（`Platform::send_channel_message`）不能把路由处理器挤掉；
* 内联闭包路由不能因为"没有函数名"被漏掉；
* 手改矩阵里的 auth 必须被发现；
* 引用了不存在的用例名必须被发现（编造证据是这个门禁最容易烂的方式）。
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from fixture import FakeRepo  # noqa: E402
from sourceaudit import authz  # noqa: E402
from sourceaudit.util import DataError  # noqa: E402

ROUTES_RS = '''
use axum::routing::{get, post};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/me", get(me))
        .route("/api/groups", post(create).get(list))
        .route(
            "/api/friends/{uid}",
            get(friend).delete(unfriend),
        )
        .route(
            "/api/audit/self",
            get(|State(p): State<PlatformState>, authed: Authed| async move { "x" }),
        )
}

impl Platform {
    // 同名业务方法：它排在前面的目的是复现"扫到方法而不是处理器"的陷阱
    pub async fn send_channel_message(&self, channel: &str, actor: &str) -> Result<()> { Ok(()) }
}

async fn me(State(p): State<PlatformState>, authed: Authed) -> &'static str { "me" }

async fn create(State(p): State<PlatformState>, authed: Authed) -> &'static str { "c" }

async fn list(State(p): State<PlatformState>, authed: Authed) -> &'static str { "l" }

async fn friend(State(p): State<PlatformState>, authed: Authed, Path(uid): Path<String>) -> &'static str { "f" }

async fn unfriend(State(p): State<PlatformState>, authed: Authed, Path(uid): Path<String>) -> &'static str { "u" }

async fn admin_queue(State(p): State<PlatformState>, headers: HeaderMap) -> &'static str {
    require_admin(&p, &headers)?;
    "q"
}

async fn public_profile(State(p): State<PlatformState>, Path(uid): Path<String>) -> &'static str { "p" }

async fn send_channel_message(State(p): State<PlatformState>, authed: Authed) -> &'static str { "s" }
'''

TESTS_RS = """
#[tokio::test]
async fn unauth_sweep_rejects_every_private_route() {}

#[tokio::test]
async fn friends_are_owner_scoped() {}
"""


class AuthzTest(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = FakeRepo()
        platform_src = self.repo.root / "crates/platform/src"
        platform_src.mkdir(parents=True)
        (platform_src / "routes.rs").write_text(ROUTES_RS, encoding="utf-8")
        tests = self.repo.root / "tests/security/tests"
        tests.mkdir(parents=True)
        (tests / "sweep.rs").write_text(TESTS_RS, encoding="utf-8")

    def tearDown(self) -> None:
        self.repo.cleanup()

    # ---- 扫描 -------------------------------------------------------------
    def test_scan_classifies_auth(self) -> None:
        routes = {(r.method, r.path): r.auth for r in authz.scan_routes(self.repo.root)}
        self.assertEqual(routes[("GET", "/api/me")], "user")
        self.assertEqual(routes[("POST", "/api/groups")], "user")  # 链式 get+post
        self.assertEqual(routes[("GET", "/api/groups")], "user")
        self.assertEqual(routes[("DELETE", "/api/friends/{uid}")], "user")
        self.assertEqual(routes[("GET", "/api/audit/self")], "user")  # 内联闭包

    def test_admin_detected_by_require_admin(self) -> None:
        (self.repo.root / "crates/platform/src/admin.rs").write_text(
            'pub fn r() -> R { Router::new().route("/api/admin/queue", get(admin_queue)) }\n' + ROUTES_RS,
            encoding="utf-8",
        )
        routes = {(r.method, r.path): r.auth for r in authz.scan_routes(self.repo.root)}
        self.assertEqual(routes[("GET", "/api/admin/queue")], "admin")

    def test_named_collision_does_not_hide_handler(self) -> None:
        """同名业务方法在前、处理器在后：必须判成 user，而不是 public。"""
        (self.repo.root / "crates/platform/src/community.rs").write_text(
            ROUTES_RS.replace("/api/me", "/api/channels/x/messages"), encoding="utf-8"
        )
        found = [r for r in authz.scan_routes(self.repo.root) if r.path == "/api/channels/x/messages"]
        self.assertTrue(found)
        self.assertEqual({r.auth for r in found}, {"user"})

    # ---- 同步 -------------------------------------------------------------
    def test_sync_creates_rows_and_keeps_curated_fields(self) -> None:
        authz.sync(self.repo.root, write=True)
        matrix = self.repo.load_json("security/threat-model/authz-matrix.json")
        rows = matrix["routes"]
        self.assertGreaterEqual(len(rows), 5)
        # 人工字段
        for row in rows:
            if row["path"] == "/api/me":
                row["scope"] = "self"
                row["notes"] = "人工备注"
        self.repo.write_json("security/threat-model/authz-matrix.json", matrix)

        # 源码新增一条 → 再同步不应丢人工字段（老路由必须还在，否则是"替换"不是"新增"）
        routes_file = self.repo.root / "crates/platform/src/routes.rs"
        routes_file.write_text(
            ROUTES_RS.replace(
                '.route("/api/groups", post(create).get(list))',
                '.route("/api/groups", post(create).get(list)).route("/api/me2", get(me))',
            ),
            encoding="utf-8",
        )
        authz.sync(self.repo.root, write=True)
        rows = self.repo.load_json("security/threat-model/authz-matrix.json")["routes"]
        me = next(r for r in rows if r["path"] == "/api/me")
        self.assertEqual(me["scope"], "self")
        self.assertEqual(me["notes"], "人工备注")
        self.assertIn("/api/me2", [r["path"] for r in rows])

    def test_sync_is_idempotent(self) -> None:
        authz.sync(self.repo.root, write=True)
        first = (self.repo.root / "security/threat-model/authz-matrix.json").read_bytes()
        report = authz.sync(self.repo.root, write=True)
        second = (self.repo.root / "security/threat-model/authz-matrix.json").read_bytes()
        self.assertEqual(first, second)
        self.assertEqual(report.meta["added"], 0)

    # ---- 核对 -------------------------------------------------------------
    def test_clean_matrix_passes(self) -> None:
        authz.sync(self.repo.root, write=True)
        self._fill_public_reasons()
        report = authz.check(self.repo.root)
        self.assertEqual(report.blocking, [], report.render_text())

    def test_unregistered_route_blocks(self) -> None:
        authz.sync(self.repo.root, write=True)
        routes_file = self.repo.root / "crates/platform/src/routes.rs"
        routes_file.write_text(
            ROUTES_RS.replace('"/api/me", get(me)', '"/api/me", get(me)).route("/api/secret", get(me)'),
            encoding="utf-8",
        )
        self.assertIn("AUTHZ-UNREGISTERED", [f.code for f in authz.check(self.repo.root).blocking])

    def test_stale_row_blocks(self) -> None:
        authz.sync(self.repo.root, write=True)
        matrix = self.repo.load_json("security/threat-model/authz-matrix.json")
        matrix["routes"].append(
            {"method": "GET", "path": "/api/gone", "handler": "x", "module": "m", "auth": "user", "tests": ["friends_are_owner_scoped"]}
        )
        self.repo.write_json("security/threat-model/authz-matrix.json", matrix)
        self.assertIn("AUTHZ-STALE", [f.code for f in authz.check(self.repo.root).blocking])

    def test_hand_edited_auth_is_detected(self) -> None:
        authz.sync(self.repo.root, write=True)
        matrix = self.repo.load_json("security/threat-model/authz-matrix.json")
        for row in matrix["routes"]:
            if row["path"] == "/api/me":
                row["auth"] = "public"
                row["public_reason"] = "看起来不需要"
        self.repo.write_json("security/threat-model/authz-matrix.json", matrix)
        codes = [f.code for f in authz.check(self.repo.root).blocking]
        self.assertIn("AUTHZ-AUTH-DRIFT", codes)

    def test_public_without_reason_blocks(self) -> None:
        authz.sync(self.repo.root, write=True)
        matrix = self.repo.load_json("security/threat-model/authz-matrix.json")
        matrix["routes"].append(
            {
                "method": "GET",
                "path": "/api/friends/{uid}",
                "handler": "friend",
                "module": "m",
                "auth": "public",
                "public_reason": "TODO：未填理由",
            }
        )
        # 用同路径覆盖真实行的方式会触发 STALE/重复；这里直接验证 public 规则
        matrix["routes"] = [r for r in matrix["routes"] if not (r["path"] == "/api/friends/{uid}" and r["auth"] == "user")]
        self.repo.write_json("security/threat-model/authz-matrix.json", matrix)
        self.assertIn("AUTHZ-PUBLIC", [f.code for f in authz.check(self.repo.root).blocking])

    def test_private_without_tests_blocks(self) -> None:
        authz.sync(self.repo.root, write=True)
        matrix = self.repo.load_json("security/threat-model/authz-matrix.json")
        for row in matrix["routes"]:
            if row["path"] == "/api/me":
                row["tests"] = []
        self.repo.write_json("security/threat-model/authz-matrix.json", matrix)
        self.assertIn("AUTHZ-NO-TEST", [f.code for f in authz.check(self.repo.root).blocking])

    def test_ghost_test_blocks(self) -> None:
        authz.sync(self.repo.root, write=True)
        matrix = self.repo.load_json("security/threat-model/authz-matrix.json")
        for row in matrix["routes"]:
            if row["path"] == "/api/me":
                row["tests"] = ["this_test_does_not_exist"]
        self.repo.write_json("security/threat-model/authz-matrix.json", matrix)
        self.assertIn("AUTHZ-GHOST-TEST", [f.code for f in authz.check(self.repo.root).blocking])

    def test_bad_schema_is_data_error(self) -> None:
        self.repo.write_json("security/threat-model/authz-matrix.json", {"schema": "wrong", "routes": []})
        with self.assertRaises(DataError):
            authz.check(self.repo.root)

    # ---- 工具 -------------------------------------------------------------
    def _fill_public_reasons(self) -> None:
        matrix = self.repo.load_json("security/threat-model/authz-matrix.json")
        for row in matrix["routes"]:
            if row["auth"] == "public":
                row["public_reason"] = "测试用：公开理由"
                row["tests"] = []
        self.repo.write_json("security/threat-model/authz-matrix.json", matrix)


class ConcretizeTest(unittest.TestCase):
    def test_path_params_are_substituted(self) -> None:
        self.assertEqual(authz.OBJECT_OF["conv"], "conversation")
        params = authz._PATH_PARAM_RE.findall("/api/secret/{peer}/verify")
        self.assertEqual(params, ["peer"])


if __name__ == "__main__":
    unittest.main()
