p = 'crates/rtc/src/host.rs'
s = open(p, encoding='utf-8').read()

# 1) 新增字段
s = s.replace('''    /// 对端媒体地址，仅用于诊断（发送地址来自 str0m 自己的 `destination`）。
    peers: HashMap<String, SocketAddr>,''', '''    /// 对端媒体地址，仅用于诊断（发送地址来自 str0m 自己的 `destination`）。
    peers: HashMap<String, SocketAddr>,
    /// 远端媒体地址 → call_id。**从出向数据报学到**，用于把入向包只投给
    /// 对应的那条会话。
    ///
    /// 为什么必须有它：多条会话共用一个 socket 时，如果把每个入向包都喂给
    /// 所有会话，非目标的会话也会收到别人的 DTLS/ICE 包，str0m 会按来源当成
    /// 新的连通性检查处理，结果是一条通了、另一条迟迟不通（甚至永久卡住）。
    /// 一对一时看不出问题，多方通话必然踩到。
    route: HashMap<SocketAddr, String>,''')

s = s.replace('''            connected: HashSet::new(),
            peers: HashMap::new(),
        }''', '''            connected: HashSet::new(),
            peers: HashMap::new(),
            route: HashMap::new(),
        }''')

# 2) handle_datagram 按来源路由
old = '''    pub fn handle_datagram(&mut self, from: SocketAddr, to: SocketAddr, bytes: &[u8]) -> Vec<Effect> {
        let mut out = Vec::new();
        let d = Datagram {
            data: bytes.to_vec(),
            source: from,
            destination: to,
        };
        for call_id in self.live_call_ids() {
            // 1:1 通话只有一个媒体会话；包不属于它会自己丢弃。
            let accepted = match self.mgr.media_mut(&call_id) {
                Some(media) => media.handle_receive(&d).is_ok(),
                None => false,
            };
            if accepted {
                out.extend(self.pump_media(&call_id));
            }
        }
        out
    }'''
new = '''    pub fn handle_datagram(&mut self, from: SocketAddr, to: SocketAddr, bytes: &[u8]) -> Vec<Effect> {
        let d = Datagram {
            data: bytes.to_vec(),
            source: from,
            destination: to,
        };
        // 已知来源就只投给那一条会话；未知来源（还没发出过包）才广播尝试
        let targets: Vec<String> = match self.route.get(&from) {
            Some(id) => vec![id.clone()],
            None => self.live_call_ids(),
        };
        let mut out = Vec::new();
        for call_id in targets {
            let accepted = match self.mgr.media_mut(&call_id) {
                Some(media) => media.handle_receive(&d).is_ok(),
                None => false,
            };
            if accepted {
                out.extend(self.pump_media(&call_id));
            }
        }
        out
    }'''
assert old in s
s = s.replace(old, new)

# 3) pump_media 里学习路由（先收集，再插入，避免借用冲突）
old_pump = '''        for d in datagrams {
            out.push(Effect::Transmit {
                to: d.destination,
                bytes: d.data,
            });
        }'''
new_pump = '''        let mut learned: Vec<SocketAddr> = Vec::with_capacity(datagrams.len());
        for d in datagrams {
            learned.push(d.destination);
            out.push(Effect::Transmit {
                to: d.destination,
                bytes: d.data,
            });
        }
        // 记住"这个远端地址属于哪条会话"，入向包据此精确投递
        for addr in learned {
            self.route.insert(addr, call_id.to_string());
        }'''
assert old_pump in s
s = s.replace(old_pump, new_pump)

# 4) Ended 时清理路由
s = s.replace('''                Action::Ended { call_id, end } => {
                    self.connected.remove(&call_id);
                    self.peers.remove(&call_id);''', '''                Action::Ended { call_id, end } => {
                    self.connected.remove(&call_id);
                    self.peers.remove(&call_id);
                    self.route.retain(|_, id| *id != call_id);''')

open(p, 'w', encoding='utf-8').write(s)
print('ok')
