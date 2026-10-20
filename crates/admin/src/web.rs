//! 单页 HTML（D1 交付项）。
//!
//! 为什么写死在二进制里而不是读磁盘目录：后台是"独立进程 + 只读库"，
//! 多一个可配置静态目录就多一种"路径配错 → 页面 404 → 看板一片空白"的
//! 静默失败。内联保证 `wxlike-admin` 一个文件就能跑起来。
//!
//! 渲染纪律：**所有来自接口的文本都走 `textContent`**，绝不拼进 HTML 字符串。
//! 用户名、群名、举报内容是用户可控的，一次 `innerHTML` 拼接就把 XSS 变成
//! "以运维身份执行写操作"——这是后台最坏的一类洞。
//! 这个约束由 `web.rs` 的测试守着（不许出现把变量插进 innerHTML 的写法）。

/// 后台页面。`{VERSION}` 由 [`page`] 填入。
const TEMPLATE: &str = r##"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<title>wxlike 运营后台 adminx</title>
<style>
:root{--bg:#0f1216;--fg:#e6e9ef;--mut:#8b95a7;--card:#171c23;--line:#242c37;--ok:#41c47f;--warn:#e0a33a;--bad:#e8635c;--accent:#5aa7ff}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 ui-sans-serif,system-ui,"PingFang SC","Microsoft YaHei",sans-serif}
header{display:flex;flex-wrap:wrap;gap:8px;align-items:center;padding:10px 16px;background:var(--card);border-bottom:1px solid var(--line);position:sticky;top:0;z-index:2}
header h1{font-size:15px;margin:0 12px 0 0;font-weight:600}
main{padding:16px;max-width:1180px;margin:0 auto}
section{background:var(--card);border:1px solid var(--line);border-radius:10px;margin:0 0 14px;overflow:hidden}
section>h2{font-size:13px;margin:0;padding:10px 12px;border-bottom:1px solid var(--line);font-weight:600;display:flex;gap:10px;align-items:center;flex-wrap:wrap}
section>h2 small{color:var(--mut);font-weight:400}
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:1px;background:var(--line)}
.card{background:var(--card);padding:10px 12px}
.card b{display:block;font-size:19px;font-variant-numeric:tabular-nums}
.card span{color:var(--mut);font-size:12px}
input,select,button,textarea{font:inherit;color:var(--fg);background:#0c1015;border:1px solid var(--line);border-radius:6px;padding:5px 8px}
input:focus,select:focus,button:focus{outline:1px solid var(--accent)}
button{cursor:pointer;background:#1c2530}
button:hover{border-color:var(--accent)}
button.bad{color:var(--bad)}
.row{display:flex;gap:6px;flex-wrap:wrap;align-items:center;padding:10px 12px;border-bottom:1px solid var(--line)}
table{width:100%;border-collapse:collapse;font-size:13px}
th,td{text-align:left;padding:6px 12px;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--mut);font-weight:500;white-space:nowrap}
td.num{text-align:right;font-variant-numeric:tabular-nums}
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px;color:#c8d3e6;word-break:break-all}
.pill{border-radius:999px;padding:1px 8px;font-size:12px;border:1px solid var(--line);color:var(--mut)}
.pill.ok{color:var(--ok)}.pill.warn{color:var(--warn)}.pill.bad{color:var(--bad)}
#status{margin-left:auto;font-size:12px;color:var(--mut)}
.hint{padding:10px 12px;color:var(--mut)}
.err{color:var(--bad)}
.hide{display:none}
footer{padding:8px 16px 24px;color:var(--mut);font-size:12px}
a{color:var(--accent)}
</style>
</head>
<body>
<header>
  <h1>wxlike 运营后台 <span class="pill">adminx {VERSION}</span></h1>
  <label>令牌 <input id="rx" type="password" size="18" placeholder="WXLIKE_ADMIN_TOKEN" autocomplete="off"></label>
  <button id="saveRx">保存</button>
  <span id="who"></span>
  <label class="hide" id="loginBox">账号 <input id="u" size="10" autocomplete="username"></label>
  <label class="hide" id="passBox">口令 <input id="p" type="password" size="12" autocomplete="current-password"></label>
  <label id="totpBox" class="hide">动态码 <input id="t" size="6" inputmode="numeric" autocomplete="one-time-code"></label>
  <button id="login">登录</button>
  <button id="logout" class="hide">退出</button>
  <span id="status"></span>
</header>
<main>
  <div id="boot" class="hint">在上方填入后台令牌（<code>WXLIKE_ADMIN_TOKEN</code>）后点「保存」。</div>

  <section id="panel" class="hide">
    <h2>看板 <small>只读，来自平台库；<span id="mode"></span></small>
      <button id="refresh">刷新</button></h2>
    <div class="cards" id="stats"></div>
    <div class="hint" id="reconcile"></div>
  </section>

  <section id="opsPanel" class="hide">
    <h2>写操作 <small>需登录且<b>已验动态码</b>的会话。每一次尝试（含被拒）都会留痕。</small></h2>
    <div class="row">
      <button data-op="wallet.reconcile">跑一次对账</button>
      <button data-op="moderation.mode" data-mode="pre_review">切：先审后发</button>
      <button data-op="moderation.mode" data-mode="post_review">切：先发后审</button>
      <span class="hint" style="padding:0" id="opsNote"></span>
    </div>
  </section>

  <section>
    <h2>用户 <small>前缀搜索，不含口令/TOTP 密钥</small></h2>
    <div class="row">
      <label>名字 <input id="uq" size="14"></label>
      <button id="uGo">查</button>
      <button id="uPrev">←</button><button id="uNext">→</button>
      <span id="uCount" class="pill"></span>
    </div>
    <div class="hint err" id="uErr"></div>
    <div id="uTable"></div>
  </section>

  <section>
    <h2>群</h2>
    <div class="row">
      <button id="gGo">刷新</button>
      <button id="gPrev">←</button><button id="gNext">→</button>
      <span id="gCount" class="pill"></span>
    </div>
    <div class="hint err" id="gErr"></div>
    <div id="gTable"></div>
  </section>

  <section>
    <h2>审计检索 <small>command 填命令 id（如 <code>group.create</code>），对得上平台落的 <code>cmd.group.create</code></small></h2>
    <div class="row">
      <label>actor <input id="aActor" size="12"></label>
      <label>command <input id="aCmd" size="14"></label>
      <label>action <input id="aAction" size="14"></label>
      <label>target <input id="aTarget" size="16"></label>
      <label>从 <input id="aFrom" size="14" placeholder="epoch ms"></label>
      <label>到 <input id="aTo" size="14" placeholder="epoch ms"></label>
      <button id="aGo">检索</button>
      <button id="a7d">最近 7 天</button>
      <button id="aPrev">←</button><button id="aNext">→</button>
      <span id="aCount" class="pill"></span>
    </div>
    <div class="hint err" id="aErr"></div>
    <div id="aTable"></div>
  </section>

  <section>
    <h2>举报工单 <small>状态机在 <code>wxlike-report</code>，这里只展示与提交流转</small></h2>
    <div class="row">
      <label>状态
        <select id="rState">
          <option value="">未终结</option>
          <option value="open">open</option>
          <option value="triaged">triaged</option>
          <option value="actioned">actioned</option>
          <option value="closed">closed</option>
          <option value="rejected">rejected</option>
        </select>
      </label>
      <label>处理人 <input id="rHandler" size="10"></label>
      <button id="rGo">刷新</button>
      <button id="rPrev">←</button><button id="rNext">→</button>
      <span id="rCount" class="pill"></span>
    </div>
    <div class="hint err" id="rErr"></div>
    <div id="rTable"></div>
  </section>

  <section>
    <h2>审核挂起队列 <small>先审后发时进来的内容</small></h2>
    <div class="row"><button id="mGo">刷新</button><span id="mCount" class="pill"></span></div>
    <div class="hint err" id="mErr"></div>
    <div id="mTable"></div>
  </section>

  <section>
    <h2>操作留痕 <small>adminx 自己的库：谁、哪条操作、结果</small></h2>
    <div class="row">
      <label>结果 <select id="oStatus"><option value="">全部</option><option value="ok">ok</option><option value="denied">denied</option><option value="upstream_error">upstream_error</option><option value="transport_error">transport_error</option></select></label>
      <button id="oGo">刷新</button><span id="oCount" class="pill"></span>
    </div>
    <div class="hint err" id="oErr"></div>
    <div id="oTable"></div>
  </section>

  <section>
    <h2>操作目录</h2>
    <div id="catalog"></div>
  </section>
  <footer>
    adminx 对平台库<b>只读</b>（<code>SQLITE_OPEN_READ_ONLY</code> + <code>query_only</code>）。
    所有会改数据的动作都转发给 <code>wxlike-server</code> 的 <code>/api/admin/*</code>，
    并在留痕库里落一行。文档：<code>docs/adminx.md</code>
  </footer>
</main>
<script>
"use strict";
var S = {rx: sessionStorage.getItem("rx") || "", sx: sessionStorage.getItem("sx") || "",
         uo: 0, go: 0, ao: 0, ro: 0, session: null};
var PAGE = 50;
function $(id){return document.getElementById(id);}
function bearer(){return S.sx || S.rx;}
function status(msg, bad){var e=$("status"); e.textContent=msg; e.className=bad?"err":"";}
function api(path, opt){
  opt = opt || {};
  var h = {"content-type":"application/json"};
  var b = bearer();
  if(b){h["authorization"]="Bearer "+b;}
  opt.headers = h;
  return fetch(path, opt).then(function(r){
    return r.text().then(function(t){
      var j = null;
      try { j = t ? JSON.parse(t) : null; } catch(e){ j = {raw:t}; }
      return {code:r.status, body:j};
    });
  });
}
function fail(where, res){
  var m = (res.body && (res.body.error || res.body.raw)) || ("HTTP "+res.code);
  where.textContent = "HTTP "+res.code+"："+m;
  return false;
}
function mk(tag, cls, text){var e=document.createElement(tag); if(cls){e.className=cls;} if(text!==undefined&&text!==null){e.textContent=String(text);} return e;}
function table(host, cols, rows, render){
  var box = $(host); box.textContent = "";
  if(!rows.length){ box.appendChild(mk("div","hint","（没有记录）")); return; }
  var t = mk("table"), thead = mk("thead"), tr = mk("tr");
  cols.forEach(function(c){tr.appendChild(mk("th","",c));});
  thead.appendChild(tr); t.appendChild(thead);
  var tb = mk("tbody");
  rows.forEach(function(r){
    var row = mk("tr");
    render(row, r);
    tb.appendChild(row);
  });
  t.appendChild(tb); box.appendChild(t);
}
function cell(row, text, cls){var td=mk("td",cls); td.appendChild(mk("code","",text)); row.appendChild(td); return td;}
function fmtTime(ms){
  if(!ms) return "—";
  var d = new Date(ms);
  return d.toISOString().replace("T"," ").slice(0,19);
}
// ---- 看板 -----------------------------------------------------------------
function loadStats(){
  return api("/api/stats").then(function(res){
    if(res.code !== 200){ return fail($("reconcile"), res); }
    var b = res.body, host = $("stats"); host.textContent = "";
    var pairs = [
      ["用户", b.users_total, "24h 新增 "+b.users_created_24h],
      ["活跃会话", b.active_sessions, ""],
      ["群", b.groups_total, ""],
      ["社区", b.communities_total, ""],
      ["信箱会话", b.conversations, "序列号合计 "+b.message_seqs],
      ["未结工单", b.reports_open, "超 24h "+b.reports_over_24h],
      ["审核挂起", b.moderation_held, ""],
      ["好友申请", b.friend_requests_pending, "待处理"],
      ["余额合计", b.wallet_sum_minor, "整数分"],
      ["审计", b.audit_in_sample_24h, "24h / 采样 "+b.audit_sample_rows+" 行"]
    ];
    pairs.forEach(function(p){
      var c = mk("div","card");
      c.appendChild(mk("b", undefined, p[1]));
      c.appendChild(mk("span", undefined, p[0]));
      if(p[2]){ c.appendChild(mk("span", undefined, p[2])); }
      host.appendChild(c);
    });
    var r = b.last_reconcile;
    $("reconcile").textContent = r
      ? ("最近对账 "+fmtTime(r.at_ms)+"：" + (r.ok ? "守恒通过" : "有 "+r.mismatches+" 处不符") + "（核对 "+r.users_checked+" 个钱包）")
      : "还没跑过对账。上方「跑一次对账」可以手动执行。";
    $("reconcile").className = "hint " + (r && !r.ok ? "err" : "");
  });
}
function loadHealth(){
  return api("/health").then(function(res){
    if(res.code !== 200){ return; }
    var b = res.body;
    $("mode").textContent = "库打开方式 " + b.db_open_mode + "；缺表 "+(b.missing_tables.length?b.missing_tables.join(", "):"无");
    if(!S.rx && !S.sx){ $("boot").classList.remove("hide"); }
  });
}
// ---- 列表页 ---------------------------------------------------------------
function loadUsers(){
  var q = encodeURIComponent($("uq").value.trim());
  return api("/api/users?limit="+PAGE+"&offset="+S.uo+(q?("&q="+q):"")).then(function(res){
    if(res.code !== 200){ return fail($("uErr"), res); }
    $("uErr").textContent = "";
    var b = res.body; $("uCount").textContent = "共 "+b.total+" 条";
    table("uTable", ["id","名字","朋友","余额","流水净额","最近活跃","注册于"], b.rows, function(row, r){
      cell(row, r.id); cell(row, r.name);
      mk(row,"td","num").appendChild(document.createTextNode(r.friends));
      mk(row,"td","num").appendChild(document.createTextNode(r.wallet_balance===null?"—":r.wallet_balance));
      mk(row,"td","num").appendChild(document.createTextNode(r.ledger_net));
      cell(row, r.last_seen?fmtTime(r.last_seen):"—");
      cell(row, fmtTime(r.created_at));
    });
  });
}
function loadGroups(){
  return api("/api/groups?limit="+PAGE+"&offset="+S.go).then(function(res){
    if(res.code !== 200){ return fail($("gErr"), res); }
    $("gErr").textContent = "";
    var b = res.body; $("gCount").textContent = "共 "+b.total+" 条";
    table("gTable", ["id","群名","群主","成员","审批","全员禁言","建于"], b.rows, function(row,r){
      cell(row, r.id); cell(row, r.name); cell(row, r.owner_uid);
      mk(row,"td","num").appendChild(document.createTextNode(r.members));
      cell(row, r.approval?"开":"关"); cell(row, r.all_muted?"是":"否");
      cell(row, fmtTime(r.created_at));
    });
  });
}
function auditQuery(){
  var p = ["limit="+PAGE, "offset="+S.ao];
  var map = {actor:"aActor", command:"aCmd", action:"aAction", target:"aTarget", from:"aFrom", to:"aTo"};
  Object.keys(map).forEach(function(k){
    var v = $(map[k]).value.trim();
    if(v){ p.push(k+"="+encodeURIComponent(v)); }
  });
  return p.join("&");
}
function loadAudit(){
  return api("/api/audit?"+auditQuery()).then(function(res){
    if(res.code !== 200){ return fail($("aErr"), res); }
    $("aErr").textContent = "";
    var b = res.body; $("aCount").textContent = "命中 "+b.total+" 条";
    table("aTable", ["时间","actor","action","target","detail","id"], b.rows, function(row,r){
      cell(row, fmtTime(r.at_ms)); cell(row, r.actor||"—"); cell(row, r.action);
      cell(row, r.target||"—"); cell(row, r.detail||"—"); cell(row, r.id);
    });
  });
}
function loadReports(){
  var p = ["limit="+PAGE, "offset="+S.ro];
  var st = $("rState").value; if(st){ p.push("state="+st); }
  var h = $("rHandler").value.trim(); if(h){ p.push("handler="+encodeURIComponent(h)); }
  return api("/api/reports?"+p.join("&")).then(function(res){
    if(res.code !== 200){ return fail($("rErr"), res); }
    $("rErr").textContent = "";
    var b = res.body; $("rCount").textContent = "共 "+b.total+" 条";
    table("rTable", ["工单","被举报","理由","状态","处理人","更新于","流转"], b.rows, function(row, r){
      var t = r.ticket;
      cell(row, r.id+" ← "+t.reporter);
      cell(row, t.target_kind+" / "+t.target_id);
      cell(row, t.reason + (t.detail?"："+"":"") + t.detail);
      var td = mk("td");
      var pill = mk("span", r.overdue?"pill warn":"pill", t.state);
      td.appendChild(pill); row.appendChild(td);
      cell(row, t.handler||"—");
      cell(row, fmtTime(t.updated_at));
      var ops = mk("td");
      [["triage","分诊"],["act","处置"],["close","结案"],["reject","驳回"]].forEach(function(o){
        var btn = mk("button", undefined, o[1]);
        btn.onclick = function(){ doReportOp(o[0], r.id); };
        ops.appendChild(btn);
      });
      row.appendChild(ops);
      if(t.history.length){
        var h2 = mk("tr");
        var span = mk("td"); span.colSpan = 7;
        span.appendChild(mk("code", undefined, t.history.map(function(x){
          return fmtTime(x.at_ms)+" "+x.from+"→"+x.to+" by "+x.actor+"："+x.note;
        }).join(" · ")));
        h2.appendChild(span);
        row.parentNode.appendChild(h2);
      }
    });
  });
}
function loadModeration(){
  return api("/api/moderation?limit="+PAGE).then(function(res){
    if(res.code !== 200){ return fail($("mErr"), res); }
    $("mErr").textContent = "";
    var b = res.body; $("mCount").textContent = "挂起 "+b.total+" 条";
    table("mTable", ["id","会话","发送者","文本","标签","挂起于","处置"], b.rows, function(row, r){
      cell(row, r.id); cell(row, r.conv_id||"—"); cell(row, r.sender);
      cell(row, r.text); cell(row, r.labels.join(", ")||"—"); cell(row, fmtTime(r.created_at));
      var ops = mk("td");
      var ok = mk("button", undefined, "放行");
      ok.onclick = function(){ runOp("moderation.release", r.id, null, "放行挂起内容"); };
      var no = mk("button","bad","丢弃");
      no.onclick = function(){ runOp("moderation.reject", r.id, null, "丢弃挂起内容（不可恢复）"); };
      ops.appendChild(ok); ops.appendChild(no); row.appendChild(ops);
    });
  });
}
function loadOplog(){
  var p = ["limit="+PAGE];
  var st = $("oStatus").value; if(st){ p.push("status="+st); }
  return api("/api/oplog?"+p.join("&")).then(function(res){
    if(res.code !== 200){ return fail($("oErr"), res); }
    $("oErr").textContent = "";
    var b = res.body; $("oCount").textContent = "共 "+b.total+" 条";
    table("oTable", ["时间","操作者","操作","目标","结果","MFA","上游","detail"], b.rows, function(row, r){
      cell(row, fmtTime(r.at_ms)); cell(row, r.actor); cell(row, r.op); cell(row, r.target||"—");
      var td = mk("td");
      td.appendChild(mk("span", r.status==="ok"?"pill ok":(r.status==="denied"?"pill warn":"pill bad"), r.status));
      row.appendChild(td);
      cell(row, r.mfa?"已验码":"未验码");
      cell(row, r.upstream||"—"); cell(row, r.detail||"—");
    });
  });
}
function loadCatalog(){
  return api("/api/ops").then(function(res){
    if(res.code !== 200){ return; }
    table("catalog", ["操作 id","需要能力","目标","载荷","上游","说明"], res.body.ops, function(row, o){
      cell(row, o.id); cell(row, o.capability); cell(row, o.needs_target?"必填":"不需要");
      cell(row, o.payload); cell(row, o.method+" "+o.path); cell(row, o.description);
    });
  });
}
// ---- 会话 -----------------------------------------------------------------
function renderSession(){
  var s = S.session;
  $("loginBox").classList.toggle("hide", !!s);
  $("passBox").classList.toggle("hide", !!s);
  $("totpBox").classList.toggle("hide", !!s);
  $("login").classList.toggle("hide", !!s);
  $("logout").classList.toggle("hide", !s);
  $("opsPanel").classList.toggle("hide", !s);
  if(!s){ $("who").textContent = ""; return; }
  $("who").textContent = "已登录 " + s.actor + "（" + s.role + "，" + (s.mfa?"已验码":"未验码") + "）";
  $("opsNote").textContent = s.mfa ? "" : "当前会话没验动态码：任何写操作都会被拒。绑码后重新登录。";
}
function doLogin(){
  var body = {username:$("u").value.trim(), password:$("p").value};
  var t = $("t").value.trim(); if(t){ body.totp = t; }
  return api("/api/login", {method:"POST", body:JSON.stringify(body)}).then(function(res){
    if(res.code !== 200){ return fail($("who"), res); }
    S.sx = res.body.token; sessionStorage.setItem("sx", S.sx);
    S.session = res.body.session;
    renderSession(); refreshAll();
  });
}
function doLogout(){
  return api("/api/logout", {method:"POST"}).then(function(){
    S.sx = ""; sessionStorage.removeItem("sx"); S.session = null;
    renderSession(); refreshAll();
  });
}
function runOp(op, target, extra, label){
  if(!S.session){ status("先登录", true); return; }
  if(!window.confirm(label + "\n\n操作：" + op + "\n目标：" + (target||"（无）"))){ return; }
  var body = extra || {};
  if(target){ body.target = target; }
  return api("/api/op/"+encodeURIComponent(op), {method:"POST", body:JSON.stringify(body)}).then(function(res){
    loadOplog();
    if(res.code !== 200){ status(op+" 失败：HTTP "+res.code, true); return refreshAll(); }
    status(op+" 已执行");
    refreshAll();
  }).catch(function(e){ status("请求异常："+e, true); });
}
function doReportOp(kind, id){
  var note = window.prompt("写一句依据（会进留痕与平台审计）：" + kind + " " + id);
  if(note === null || !note.trim()){ status("没写依据，已取消", true); return; }
  if(kind !== "act"){ return runOp("report."+kind, id, {note:note.trim()}, "工单 "+kind); }
  var raw = window.prompt("处置动作 JSON：{\"action\":\"warn\"} 或 {\"action\":\"mute\",\"duration_ms\":60000} 或 ban / remove_content / none", "{\"action\":\"warn\"}");
  if(raw === null){ return; }
  var parsed;
  try { parsed = JSON.parse(raw); } catch(e){ status("处置动作不是合法 JSON，已取消", true); return; }
  parsed.note = note.trim();
  return runOp("report.act", id, parsed, "工单处置 "+id);
}
// ---- 装配 ------------------------------------------------------------------
function refreshAll(){
  if(!bearer()){ return; }
  $("boot").classList.add("hide");
  $("panel").classList.remove("hide");
  api("/health").then(function(res){
    if(res.code === 200 && res.body.missing_tables.length){
      status("平台库缺表：" + res.body.missing_tables.join(", "), true);
    }
  });
  loadHealth(); loadStats(); loadUsers(); loadGroups(); loadAudit();
  loadReports(); loadModeration(); loadOplog(); loadCatalog();
}
$("saveRx").onclick = function(){
  S.rx = $("rx").value; sessionStorage.setItem("rx", S.rx); S.sx=""; sessionStorage.removeItem("sx");
  S.session=null; renderSession(); refreshAll();
};
$("login").onclick = doLogin;
$("logout").onclick = doLogout;
$("refresh").onclick = refreshAll;
$("uGo").onclick = function(){ S.uo=0; loadUsers(); };
$("uPrev").onclick = function(){ S.uo=Math.max(0,S.uo-PAGE); loadUsers(); };
$("uNext").onclick = function(){ S.uo+=PAGE; loadUsers(); };
$("gGo").onclick = function(){ loadGroups(); };
$("gPrev").onclick = function(){ S.go=Math.max(0,S.go-PAGE); loadGroups(); };
$("gNext").onclick = function(){ S.go+=PAGE; loadGroups(); };
$("aGo").onclick = function(){ S.ao=0; loadAudit(); };
$("aPrev").onclick = function(){ S.ao=Math.max(0,S.ao-PAGE); loadAudit(); };
$("aNext").onclick = function(){ S.ao+=PAGE; loadAudit(); };
$("a7d").onclick = function(){
  $("aFrom").value = String(Date.now()-7*864e5); $("aTo").value="";
  S.ao=0; loadAudit();
};
$("rGo").onclick = function(){ S.ro=0; loadReports(); };
$("rPrev").onclick = function(){ S.ro=Math.max(0,S.ro-PAGE); loadReports(); };
$("rNext").onclick = function(){ S.ro+=PAGE; loadReports(); };
$("mGo").onclick = loadModeration;
$("oGo").onclick = loadOplog;
Array.prototype.forEach.call(document.querySelectorAll("[data-op]"), function(btn){
  btn.onclick = function(){
    var op = btn.getAttribute("data-op");
    if(op === "moderation.mode"){ runOp(op, null, {mode: btn.getAttribute("data-mode")}, "切换审核模式"); }
    else { runOp(op, null, {}, "跑一次钱包对账"); }
  };
});
if(S.rx){ $("rx").value = S.rx; }
renderSession();
api("/health").then(function(){ if(bearer()){ refreshAll(); } else { $("boot").classList.remove("hide"); } });
</script>
</body>
</html>
"##;

/// 填入版本号。页面里唯一一次字符串插值就是这个常量。
pub fn page(version: &str) -> String {
    TEMPLATE.replace("{VERSION}", version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_renders_with_the_version_and_no_leftover_placeholder() {
        let html = page("0.1.0");
        assert!(html.starts_with("<!doctype html>"), "必须是完整文档");
        assert!(html.contains("adminx 0.1.0"), "{html}");
        assert!(!html.contains("{VERSION}"), "占位符必须被替换干净");
        assert!(html.len() > 4_000, "页面不该被截断");
    }

    /// XSS 边界：来自接口的数据只能走 `textContent`。
    /// 这条断言拦的是"以后有人图省事拼 innerHTML"。
    #[test]
    fn no_innerhtml_string_building_for_api_data() {
        let lower = TEMPLATE.to_lowercase();
        assert!(
            !lower.contains("innerhtml"),
            "页面不许用 innerHTML 写接口数据：见 web.rs 的渲染纪律"
        );
        assert!(!lower.contains("document.write"), "同样禁止 document.write");
        assert!(lower.contains("textcontent"), "必须用 textContent");
    }

    /// 页面调的接口是否都注册着，由 `tests/d1_read_only.rs` 拿**真路由**逐个打一遍
    /// 来证明。这里只断言"路径清单没被改乱"，不假装在校验路由。
    #[test]
    fn page_only_references_adminx_paths() {
        let html = page("0.1.0");
        assert!(html.contains("fetch("), "页面必须用 fetch 打接口");
        assert!(
            !html.contains("http://") && !html.contains("https://"),
            "单页不许引用外部地址：后台数据敏感，也不该有第三方加载点"
        );
    }
}
