//! AR-4/H-7 + M-3（第四轮审计）：JS 宿主的执行边界。
//!
//! 页面包是不可信代码——宿主必须自己设边界：
//! * 死循环不能冻结宿主线程（loop iteration limit）
//! * `call_global` 的函数名只是属性查找，绝不拼进 JS 源码执行

use serde_json::json;
use wxlike_container::engine::BoaEngine;

#[test]
fn infinite_loop_throws_instead_of_freezing() {
    let mut engine = BoaEngine::new();
    let err = engine
        .eval("let x = 0; while (true) { x += 1; } x")
        .expect_err("an unbounded loop must throw, not hang");
    assert!(
        err.to_string().to_lowercase().contains("iteration"),
        "expected a loop-iteration error, got: {err}"
    );
}

#[test]
fn bounded_loops_still_run_to_completion() {
    let mut engine = BoaEngine::new();
    let out = engine
        .eval(
            "let s = 0; for (let i = 0; i < 500000; i++) { s += i; } s",
        )
        .expect("a legitimate half-million-iteration loop must pass");
    assert!(out.contains("124999750000"), "sum computed correctly: {out}");
}

#[test]
fn crafted_function_name_is_a_lookup_not_code() {
    let mut engine = BoaEngine::new();
    engine
        .eval("globalThis.pwned = false;")
        .expect("setup eval");
    // With the old eval-based marshalling this name executed the trailing
    // statement. Now it can only fail the property lookup.
    let err = engine
        .call_global("console.log(1); globalThis.pwned = true; //", &[])
        .expect_err("a crafted name must not resolve to a function");
    assert!(
        err.to_string().contains("is not a function"),
        "expected lookup failure, got: {err}"
    );
    let pwned = engine
        .eval("String(globalThis.pwned)")
        .expect("read back pwned");
    assert_eq!(pwned.trim(), "false", "no code from the name may run");
}

#[test]
fn string_arguments_are_data_not_source() {
    let mut engine = BoaEngine::new();
    engine
        .eval("globalThis.pwned = false; globalThis.echo = (s) => s;")
        .expect("define echo");
    // the `)` and quotes would break out of the old format!-built call site
    let out = engine
        .call_global("echo", &[json!("a); globalThis.pwned = true; //")])
        .expect("call echo with hostile string");
    assert!(
        out.contains("pwned = true"),
        "the payload must round-trip as data: {out}"
    );
    let pwned = engine.eval("String(globalThis.pwned)").unwrap();
    assert_eq!(pwned.trim(), "false", "the argument never executed");
}
