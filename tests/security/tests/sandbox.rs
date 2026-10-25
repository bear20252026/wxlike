//! G17-04（沙箱面）：小程序页包的执行边界。
//!
//! 三件事必须成立，否则"沙箱"只是名字：
//! 1. 死循环不能挂住宿主（循环预算）；
//! 2. 页面拿不到宿主能力（没有 `require`/`process`/`fetch`/文件系统）；
//! 3. `call_global` 的名字与参数**不能变成代码**（历史 AR-4/M-3 的注入点）。
//!
//! 全部打真实 `wxlike-container`（Boa 引擎）与 `wxlike-dpage`（页包），
//! 不 mock 引擎——那会把"引擎有没有这个洞"这件事测没。

use std::path::{Path, PathBuf};
use wxlike_container::{BoaEngine, Container};
use wxlike_dpage::{pack_dir, unpack};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-sbx-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_page(dir: &Path, main_js: &str) {
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"name":"sbx","main_js":"main.js","title":""}"#,
    )
    .unwrap();
    std::fs::write(dir.join("main.js"), main_js).unwrap();
}

/// SANDBOX-001：`while (true) {}` 必须在循环预算内报错，而不是永久占用线程。
#[test]
fn infinite_loop_hits_the_budget() {
    let mut engine = BoaEngine::new();
    let started = std::time::Instant::now();
    let err = engine
        .eval("while (true) {}")
        .expect_err("死循环应当被预算打断");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "打断死循环花了 {:?}：循环预算没生效或被设得过大",
        started.elapsed()
    );
    // 打断后引擎仍可用（不是把上下文搞坏了）
    let ok = engine.eval("1 + 1").expect("预算触发后引擎应仍可用");
    assert!(ok.contains('2'), "打断后求值异常：{ok}");
    let _ = err;
}

/// SANDBOX-002：`for(;;)` 与嵌套循环同样受限（不能只挡住一种写法）。
#[test]
fn all_loop_shapes_are_bounded() {
    for code in [
        "for (;;) {}",
        "let i = 0; while (i < 1e18) { i += 1; }",
        "let n = 0; for (;;) { for (;;) { n += 1; } }",
        "do {} while (true)",
    ] {
        let mut engine = BoaEngine::new();
        assert!(
            engine.eval(code).is_err(),
            "{code} 应当被循环预算打断，实际正常返回"
        );
    }
}

/// SANDBOX-003：宿主能力不存在。页面能看见的只有 `wxhost` 这一个桥。
#[test]
fn host_capabilities_are_absent() {
    let mut engine = BoaEngine::new();
    for name in [
        "require",
        "process",
        "fetch",
        "XMLHttpRequest",
        "Deno",
        "Bun",
    ] {
        let probe = format!("typeof {name}");
        let out = engine.eval(&probe).expect("typeof 探测应当可求值");
        assert!(
            out.contains("undefined"),
            "页面能看见 {name}（typeof={out}）：沙箱边界被穿透"
        );
    }
    let host = engine.eval("typeof wxhost").expect("wxhost 应当存在");
    assert!(
        !host.contains("undefined"),
        "桥对象 wxhost 不存在，页面无法与宿主交互：{host}"
    );
}

/// SANDBOX-004：`call_global` 的名字与参数不能变成代码（AR-4/M-3 回归）。
///
/// 历史实现是 `format!("{name}({args})")` + eval，一个带 `)` 的名字就能执行
/// 任意 JS。这里用"名字里塞死循环"作探针：如果它被当成源码执行，
/// 调用会卡住或抛循环预算错误，而不是"not a function"。
#[test]
fn call_global_never_evals_name_or_args() {
    let mut engine = BoaEngine::new();
    let evil_name = "onLoad); while(true){}; (function(){";
    let err = engine
        .call_global(evil_name, &[])
        .expect_err("恶意函数名不应被当成源码执行");
    let msg = format!("{err}");
    assert!(
        msg.contains("not a function"),
        "期望 'not a function'（属性查找失败），实际：{msg}"
    );

    // 参数里塞引号/分号也不能变成代码
    engine.eval("function echo(x) { return x; }").unwrap();
    let out = engine
        .call_global("echo", &[serde_json::json!("'); while(true){}; ('")])
        .expect("合法函数 + 恶意字符串参数应当正常返回");
    assert!(
        out.contains("while(true)"),
        "字符串参数应当原样往返，实际：{out}"
    );
}

/// SANDBOX-005：整条页包链路（pack -> unpack -> container）也受预算约束。
#[test]
fn page_with_deadloop_fails_to_start() {
    let dir = tmpdir("loop");
    write_page(
        &dir,
        "function onLoad(){} function onShow(){} while (true) {}",
    );
    let blob = pack_dir(&dir).expect("打包");
    let page = unpack(&blob).expect("良性包应当可解");
    let mut container = Container::load(page).expect("加载页包");
    let started = std::time::Instant::now();
    assert!(
        container.start("{}").is_err(),
        "入口脚本死循环应当使 start 失败"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "start 花了 {:?}：预算没生效",
        started.elapsed()
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// SANDBOX-006：`onLoad` 里的死循环与 `onShow` 抛错都要变成 `Err`，
/// 不能让宿主 panic（panic 会带走整个进程）。
#[test]
fn lifecycle_hooks_are_bounded_and_fallible() {
    let dir = tmpdir("hooks");
    write_page(
        &dir,
        "function onLoad() { while (true) {} } function onShow() {}",
    );
    let page = unpack(&pack_dir(&dir).unwrap()).unwrap();
    let mut c = Container::load(page).unwrap();
    assert!(c.start("{}").is_err(), "onLoad 死循环应当失败");
    std::fs::remove_dir_all(&dir).ok();

    let dir = tmpdir("throw");
    write_page(
        &dir,
        "function onLoad(){} function onShow(){ throw new Error('boom'); }",
    );
    let page = unpack(&pack_dir(&dir).unwrap()).unwrap();
    let mut c = Container::load(page).unwrap();
    assert!(c.start("{}").is_err(), "onShow 抛错应当失败而不是 panic");
    std::fs::remove_dir_all(&dir).ok();
}

/// SANDBOX-007：#253 修复后的独立回归，验证最新日志、行长及 drain 语义。
#[test]
fn bridge_state_is_bounded() {
    let mut engine = BoaEngine::new();
    engine
        .eval("for (let i = 0; i < 10000; i++) { wxhost.log('line ' + i); }")
        .expect("日志突发写入应成功");
    let logs = engine.logs();
    assert_eq!(logs.len(), 256);
    assert_eq!(logs.first().unwrap(), "line 9744");
    assert_eq!(logs.last().unwrap(), "line 9999");
    engine
        .eval("wxhost.log('你'.repeat(10000));")
        .expect("超长 Unicode 日志应受限");
    let logs = engine.drain_logs();
    assert_eq!(logs.len(), 256);
    assert!(logs.iter().all(|line| line.len() <= 4096));
    assert!(logs.last().unwrap().chars().all(|ch| ch == '你'));
    assert!(!logs.last().unwrap().is_empty());
    assert!(engine.logs().is_empty());
}

/// SANDBOX-008：缺入口脚本 / 缺 manifest 的包在加载期就失败，不留半初始化状态。
#[test]
fn broken_packages_fail_at_load() {
    let dir = tmpdir("nojs");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"name":"x","main_js":"missing.js","title":""}"#,
    )
    .unwrap();
    // 只写 manifest，不写 main.js：pack_dir 能打，但 start 必须失败
    let page = unpack(&pack_dir(&dir).unwrap()).expect("包本身合法");
    let mut c = Container::load(page).expect("加载阶段不跑 JS");
    assert!(c.start("{}").is_err(), "缺入口脚本应当 start 失败");
    std::fs::remove_dir_all(&dir).ok();

    let dir = tmpdir("nomanifest");
    std::fs::write(dir.join("main.js"), "1").unwrap();
    assert!(pack_dir(&dir).is_err(), "缺 manifest.json 应当打包失败");
    std::fs::remove_dir_all(&dir).ok();
}
