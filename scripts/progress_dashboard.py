#!/usr/bin/env python3
# 进展看板生成器：拉取 GitHub PR/issue + 本地工作树状态 -> 交互式 HTML 看板
# 用法: python scripts/progress_dashboard.py [输出路径]
import json, csv, re, glob, os, subprocess, collections, datetime, sys

REPO = 'bear20252026/wxlike'
OUT = sys.argv[1] if len(sys.argv) > 1 else r'D:\zcode1\.zcode\v2\visualizations\352095f735cc1e3fbec4c2f20e8969396cf8969a1a0d77541d5affa5b68b6065\wxlike-progress.html'

def gh(*args):
    return json.loads(subprocess.check_output(['gh', *args], text=True))

prs = gh('pr', 'list', '--state', 'all', '--limit', '300', '--json', 'number,state,title,body,headRefName,baseRefName')
issues = {it['number']: it['state'] for it in gh('issue', 'list', '--state', 'all', '--limit', '300', '--json', 'number,state')}
cards = list(csv.DictReader(open('docs/full-platform-delivery/task_board.csv', encoding='utf-8')))

# 远程分支扫描：已推送但未开 PR 的组分支（检测盲区修复 2026-10-25）
subprocess.run(['git', 'fetch', '-q', 'origin', '+refs/heads/feat/FP-G*:refs/remotes/origin/feat/FP-G*'], check=False)
pr_heads = {p['headRefName'] for p in prs if p['state'] == 'OPEN'}
pushed_refs = collections.defaultdict(set)  # -> {('#126','G05-01',...) -> {branch_sha,...}}
merged_branch_refs = set()  # 分支内容已是 main 祖先（如恢复链尾/中间分支）
for line in subprocess.check_output(['git', 'ls-remote', '--heads', 'origin', 'feat/FP-G*'], text=True).splitlines():
    sha, _, ref = line.partition('\t')
    br = ref.replace('refs/heads/', '')
    if subprocess.run(['git', 'merge-base', '--is-ancestor', sha, 'origin/main'], capture_output=True).returncode == 0:
        merged_branch_refs.add(br)
        continue  # 内容已全部进入 main，不作为"仅推送"信号
    if br in pr_heads:
        continue  # 有 open PR 的走 REVIEW 路径
    try:
        msgs = subprocess.check_output(['git', 'log', sha, '--not', 'origin/main', '--format=%s %b'], text=True,
                                       stderr=subprocess.DEVNULL)
    except Exception:
        continue
    for m in set(int(x) for x in re.findall(r'#(\d+)', msgs)):
        pushed_refs['#' + str(m)].add(br)
    for m in set(re.findall(r'G\d{2}-\d{2}', msgs)):
        pushed_refs[m.upper()].add(br)
    m = re.search(r'feat/FP-(G\d+)', br)
    if m:
        pushed_refs['GROUP:' + m.group(1)].add(br)
wt = {}
for d in glob.glob(r'D:\mimo\test\wxlike-g*'):
    g = os.path.basename(d).replace('wxlike-', '').upper()
    try:
        b = subprocess.check_output(['git', '-C', d, 'branch', '--show-current'], text=True, stderr=subprocess.DEVNULL).strip()
        t = subprocess.check_output(['git', '-C', d, 'log', '-1', '--format=%cd', '--date=format:%m-%d %H:%M'], text=True, stderr=subprocess.DEVNULL).strip()
        wt[g] = {'branch': b, 'last': t}
    except Exception:
        pass

pr_refs = collections.defaultdict(list); pr_card = collections.defaultdict(list)
for p in prs:
    txt = ((p.get('title') or '') + ' ' + (p.get('body') or '')[:2000]); st = p['state']
    # 事故教训（2026-10-25）：子 PR 合入父分支也算 MERGED，但内容不在 main——只认 base=main
    if st == 'MERGED' and p.get('baseRefName') != 'main':
        continue
    for r in set(int(x) for x in re.findall(r'#(\d+)', txt)): pr_refs[r].append((p['number'], st))
    for cid in set(re.findall(r'G\d{2}-\d{2}', txt)): pr_card[cid.upper()].append((p['number'], st))

today = datetime.date.today().strftime('%m-%d')
stat = collections.defaultdict(collections.Counter); detail = {}
for c in cards:
    grp = c['group']; cid = c['id'].upper(); iss = int(c['github_issue_number']) if c['github_issue_number'] else None
    pl = list(pr_refs.get(iss, [])) + list(pr_card.get(cid, []))
    st = 'PLANNED'
    if any(s == 'MERGED' for _, s in pl): st = 'MERGED'
    elif any(s == 'OPEN' for _, s in pl): st = 'REVIEW'
    elif iss and issues.get(iss) == 'closed': st = 'MERGED'
    elif pushed_refs.get('#' + str(iss)) or pushed_refs.get(cid) or pushed_refs.get('GROUP:' + grp):
        st = 'PUSHED'
    if st == 'PLANNED' and grp in wt and wt[grp]['last'][:5] in (today, datetime.date.fromordinal(datetime.date.today().toordinal() - 1).strftime('%m-%d')):
        st = 'LOCAL'
    stat[grp][st] += 1
    detail.setdefault(grp, []).append({'card': c['id'], 'title': c['title'], 'issue': iss, 'status': st, 'prs': sorted(set(n for n, _ in pl))})

tot = collections.Counter()
for g in stat: tot.update(stat[g])
open_prs = sum(1 for p in prs if p['state'] == 'OPEN'); merged_prs = sum(1 for p in prs if p['state'] == 'MERGED')
withpr = tot['MERGED'] + tot['REVIEW']
commits_raw = subprocess.check_output(['git', 'log', '--format=%ad', '--date=short', '--since=2026-10-14'], text=True).split()
commits = dict(sorted(collections.Counter(commits_raw).items()))
commit_tot = sum(commits.values())

order = ['G00','G01','G02','G03','G04','G05','G06','G07','G08','G09','G10','G11','G12','G13','G14','G15','G16','G17']
dom = {'G00':'地基契约','G01':'共享核心','G02':'身份安全','G03':'消息可靠','G04':'媒体文件','G05':'朋友圈文章','G06':'商业支付','G07':'扫码AI服务','G08':'小程序','G09':'RTC通话','G10':'桌面端','G11':'UI v2','G12':'移动端','G13':'内容直播','G14':'运营后台','G15':'AI命令面','G16':'CI发布','G17':'审计安全'}
stamp = datetime.datetime.now().strftime('%Y-%m-%d %H:%M')
payload = {'updated': stamp, 'prs': {'open': open_prs, 'merged': merged_prs}, 'commits': commits, 'groups': [
    {'g': g, 'domain': dom.get(g, ''), 'cards': detail[g], 'branch': wt.get(g, {}).get('branch', ''), 'last': wt.get(g, {}).get('last', '')} for g in order]}

tpl = '''<div id="wxlike-progress">
<h2 style="margin-bottom:2px">wxlike 全平台交付进度 <span class="text-muted text-small">100 卡 · 18 组 · __STAMP__</span></h2>
<div class="viz-row" style="gap:16px;flex-wrap:wrap">
  <span class="text-small">PR 管线：<b>__OPEN__</b> 待评审 · <b>__MG__</b> 已合入 · 共 __ALLPR__</span>
  <span class="text-small">有 PR 覆盖的卡：<b>__WITHPR__</b>/100</span>
  <span class="text-small">main 提交（近 10 日）：<b>__CTOT__</b></span>
</div>
<div class="viz-grid" style="margin-top:10px" id="stats"></div>
<div class="viz-row" style="margin-top:14px;align-items:center;gap:20px;flex-wrap:wrap">
  <svg id="donut" width="170" height="170" viewBox="0 0 170 170" role="img" aria-label="总体卡状态环图"><title>总体状态分布</title></svg>
  <div id="legend" style="display:flex;flex-direction:column;gap:6px"></div>
</div>
<h3 style="margin:18px 0 6px">按组推进</h3>
<div id="bars" style="display:flex;flex-direction:column;gap:6px"></div>
<h3 style="margin:18px 0 4px">卡片热力网格 <span class="text-muted text-small">点击卡片查看明细与 PR 链接</span></h3>
<div id="sel" class="card" style="margin:6px 0;padding:8px 12px" aria-live="polite">未选择卡片</div>
<div id="grid" style="display:flex;flex-direction:column;gap:8px"></div>
<h3 style="margin:18px 0 6px">main 分支提交活动（每日）</h3>
<div id="tl" style="display:flex;align-items:flex-end;gap:4px;height:110px"></div>
</div>
<style>
#wxlike-progress .st-MERGED{background:color-mix(in srgb,var(--green) 26%,transparent)}
#wxlike-progress .st-REVIEW{background:color-mix(in srgb,var(--blue) 26%,transparent)}
#wxlike-progress .st-LOCAL{background:color-mix(in srgb,var(--orange) 30%,transparent)}
#wxlike-progress .st-PLANNED{background:var(--muted)}
#wxlike-progress .st-PUSHED{background:color-mix(in srgb,var(--purple) 24%,transparent)}
#wxlike-progress .seg-MERGED{background:var(--green)}
#wxlike-progress .seg-REVIEW{background:var(--blue)}
#wxlike-progress .seg-LOCAL{background:var(--orange)}
#wxlike-progress .seg-PLANNED{background:var(--muted)}
#wxlike-progress .seg-PUSHED{background:var(--purple)}
#wxlike-progress .track{background:color-mix(in srgb,var(--muted) 45%,transparent);border-radius:7px;height:14px;flex:1;display:flex;overflow:hidden}
#wxlike-progress .brow{display:flex;align-items:center;gap:10px}
#wxlike-progress .glabel{width:170px;flex:none;font-size:12px}
#wxlike-progress .cells{display:flex;gap:6px;flex-wrap:wrap}
#wxlike-progress .cell{min-width:66px;padding:4px 2px;font-size:12px;border:1px solid transparent;border-radius:8px}
#wxlike-progress .tlbar{flex:1;display:flex;flex-direction:column;justify-content:flex-end;align-items:center;gap:3px;height:100%}
#wxlike-progress .tlfill{width:70%;border-radius:4px 4px 0 0;background:var(--viz-series-1)}
</style>
<script>
(function(){
var D=__DATA__;
var SN={MERGED:'已合入',REVIEW:'待评审',PUSHED:'已推送待提PR',LOCAL:'本地进行中',PLANNED:'未启动'};
var COL={MERGED:'var(--green)',REVIEW:'var(--blue)',PUSHED:'var(--purple)',LOCAL:'var(--orange)',PLANNED:'var(--muted)'};
var TOT={MERGED:0,REVIEW:0,PUSHED:0,LOCAL:0,PLANNED:0};
D.groups.forEach(function(g){g.cards.forEach(function(c){TOT[c.status]+=1;});});
var stats=document.getElementById('stats');
[['已合入','MERGED'],['待评审','REVIEW'],['已推送待提PR','PUSHED'],['本地进行中','LOCAL'],['未启动','PLANNED']].forEach(function(p){
  var c=document.createElement('div');c.className='card viz-stat';
  c.innerHTML='<div class="text-muted text-small">'+p[0]+'</div><div class="viz-stat-value tabular-nums">'+TOT[p[1]]+'</div>';
  stats.appendChild(c);
});
var svg=document.getElementById('donut');
var cx=85,cy=85,r=68,ri=46,a0=-Math.PI/2;
Object.keys(TOT).forEach(function(k){
  var frac=TOT[k]/100,a1=a0+frac*2*Math.PI;
  var large=(a1-a0)>Math.PI?1:0;
  var p1x=cx+r*Math.cos(a0),p1y=cy+r*Math.sin(a0),p2x=cx+r*Math.cos(a1),p2y=cy+r*Math.sin(a1);
  var p3x=cx+ri*Math.cos(a1),p3y=cy+ri*Math.sin(a1),p4x=cx+ri*Math.cos(a0),p4y=cy+ri*Math.sin(a0);
  var path=document.createElementNS('http://www.w3.org/2000/svg','path');
  path.setAttribute('d','M'+p1x+' '+p1y+' A'+r+' '+r+' 0 '+large+' 1 '+p2x+' '+p2y+' L'+p3x+' '+p3y+' A'+ri+' '+ri+' 0 '+large+' 0 '+p4x+' '+p4y+' Z');
  path.setAttribute('fill',COL[k]);
  var t=document.createElementNS('http://www.w3.org/2000/svg','title');t.textContent=SN[k]+' '+TOT[k];path.appendChild(t);
  svg.appendChild(path);a0=a1+0.0001;
});
var pct=Math.round((TOT.MERGED+TOT.REVIEW));
var tx=document.createElementNS('http://www.w3.org/2000/svg','text');
tx.setAttribute('x',85);tx.setAttribute('y',82);tx.setAttribute('text-anchor','middle');tx.setAttribute('fill','var(--foreground)');tx.setAttribute('font-size','22');
tx.textContent=pct+'%';svg.appendChild(tx);
var tx2=document.createElementNS('http://www.w3.org/2000/svg','text');
tx2.setAttribute('x',85);tx2.setAttribute('y',100);tx2.setAttribute('text-anchor','middle');tx2.setAttribute('fill','var(--muted-foreground)');tx2.setAttribute('font-size','11');
tx2.textContent='卡有 PR';svg.appendChild(tx2);
var lg=document.getElementById('legend');
Object.keys(TOT).forEach(function(k){
  var row=document.createElement('div');row.style.cssText='display:flex;align-items:center;gap:8px;font-size:12px';
  var sw=document.createElement('span');sw.style.cssText='width:12px;height:12px;border-radius:3px;background:'+COL[k];
  row.appendChild(sw);row.appendChild(document.createTextNode(SN[k]+' '+TOT[k]+' 卡'));
  lg.appendChild(row);
});
var bars=document.getElementById('bars');
D.groups.forEach(function(g){
  var n=g.cards.length;
  var cnt={MERGED:0,REVIEW:0,PUSHED:0,LOCAL:0,PLANNED:0};
  g.cards.forEach(function(c){cnt[c.status]+=1;});
  var row=document.createElement('div');row.className='brow';
  var lb=document.createElement('div');lb.className='glabel';
  lb.innerHTML='<b>'+g.g+'</b> <span class="text-muted">'+g.domain+'</span> <span class="tabular-nums text-muted">'+cnt.MERGED+'/'+n+'</span>';
  var tr=document.createElement('div');tr.className='track';
  ['MERGED','REVIEW','PUSHED','LOCAL','PLANNED'].forEach(function(k){
    if(cnt[k]===0)return;
    var s=document.createElement('div');s.className='seg-'+k;s.style.width=(cnt[k]/n*100)+'%';
    tr.appendChild(s);
  });
  var tip=[];Object.keys(cnt).forEach(function(k){tip.push(SN[k]+' '+cnt[k]);});
  tr.setAttribute('data-tooltip',g.g+' '+g.domain+'：'+tip.join(' · ')+(g.last?'（分支 '+g.branch+' @ '+g.last+'）':''));
  row.appendChild(lb);row.appendChild(tr);bars.appendChild(row);
});
var sel=document.getElementById('sel'),grid=document.getElementById('grid');
function select(card,g){
  var prs=(card.prs&&card.prs.length)?card.prs.map(function(n){return '<a href="https://github.com/bear20252026/wxlike/pull/'+n+'">#'+n+'</a>';}).join(' '):'无';
  var iss=card.issue?' · <a href="https://github.com/bear20252026/wxlike/issues/'+card.issue+'">issue #'+card.issue+'</a>':'';
  sel.innerHTML='<b>'+card.card+'</b> '+card.title+' · <b>'+SN[card.status]+'</b> · PR：'+prs+iss+' · '+g.g+' '+g.domain;
  if(typeof window.zcode!=='undefined'&&window.zcode.setWidgetState){window.zcode.setWidgetState({modelContent:{sel:card.card,status:card.status},privateContent:{sel:card.card}}).catch(function(){});}
}
D.groups.forEach(function(g){
  var row=document.createElement('div');row.className='brow';row.style.alignItems='flex-start';
  var lb=document.createElement('div');lb.className='glabel';lb.innerHTML='<b>'+g.g+'</b> <span class="text-muted text-small">'+g.domain+'</span>';
  var cells=document.createElement('div');cells.className='cells';
  g.cards.forEach(function(c){
    var b=document.createElement('button');b.type='button';b.className='btn cell st-'+c.status;
    b.textContent=c.card;b.setAttribute('aria-pressed','false');
    b.setAttribute('data-tooltip',c.card+' '+c.title+'（'+SN[c.status]+'）');
    b.onclick=function(){
      cells.querySelectorAll('.cell').forEach(function(x){x.setAttribute('aria-pressed','false');});
      b.setAttribute('aria-pressed','true');select(c,g);
    };
    cells.appendChild(b);
  });
  row.appendChild(lb);row.appendChild(cells);grid.appendChild(row);
});
var tl=document.getElementById('tl');
var days=Object.keys(D.commits).sort(),mx=Math.max.apply(null,days.map(function(d){return D.commits[d];}));
days.forEach(function(d){
  var w=document.createElement('div');w.className='tlbar';
  var f=document.createElement('div');f.className='tlfill';f.style.height=(D.commits[d]/mx*72)+'px';
  f.setAttribute('data-tooltip',d+'：'+D.commits[d]+' 次提交');
  var l=document.createElement('div');l.className='text-small text-muted';l.textContent=d.slice(5);
  var v=document.createElement('div');v.className='text-small tabular-nums';v.textContent=D.commits[d];
  w.appendChild(f);w.appendChild(l);w.appendChild(v);tl.appendChild(w);
});
})();
</script>'''
html = (tpl.replace('__STAMP__', stamp).replace('__OPEN__', str(open_prs)).replace('__MG__', str(merged_prs))
           .replace('__ALLPR__', str(len(prs))).replace('__WITHPR__', str(withpr)).replace('__CTOT__', str(commit_tot))
           .replace('__DATA__', json.dumps(payload, ensure_ascii=False, separators=(',', ':'))))
os.makedirs(os.path.dirname(OUT), exist_ok=True)
open(OUT, 'w', encoding='utf-8').write(html)
print('STAT:', dict(tot), '| PR open', open_prs, 'merged', merged_prs, '| commits10d', commit_tot)
for g in order:
    s = stat[g]
    print(f"  {g}: 合入{s['MERGED']} 评审{s['REVIEW']} 已推{s['PUSHED']} 本地{s['LOCAL']} 未启{s['PLANNED']}  @{wt.get(g,{}).get('last','-')}")
print('written', OUT)
