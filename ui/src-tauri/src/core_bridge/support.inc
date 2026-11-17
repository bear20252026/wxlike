// support 装配入口（F4 拆分，2026-11-17）：原 1338 行单文件按职责拆成 6 个
// 碎片，这里按原文本顺序 include，语义与拆分前逐字节等价。
// include! 是文本替换：碎片间的函数/常量互相可见，顺序不影响编译，
// 但保持原顺序便于与旧版本对照。守卫（catalog_drift / check-core-transport）
// 读取全部碎片拼接后的完整源码。
include!("support_state.inc");
include!("support_events.inc");
include!("support_subscribe.inc");
include!("support_session.inc");
include!("support_http.inc");
include!("support_params.inc");
