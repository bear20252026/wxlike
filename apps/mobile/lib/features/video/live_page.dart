/// G13-02：直播。
///
/// 开播/停播、进入/离开、弹幕都走 `live.*`；观看人数与状态来自服务端事件
/// （`updates`），本地计时器不产生"看起来在直播"的状态。离页即退房并取消
/// 订阅，避免后台继续占着推流或观看位。
library;

import 'dart:async';

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';
import 'channels_page.dart' show MediaSurfaceBuilder;

class LivePage extends StatefulWidget {
  const LivePage({
    super.key,
    required this.ports,
    required this.channel,
    this.host = false,
    this.playerBuilder,
  });

  final G13Ports ports;
  final String channel;

  /// 主播进入时带 true：页面会尝试开播，而不是仅观看。
  final bool host;
  final MediaSurfaceBuilder? playerBuilder;

  @override
  State<LivePage> createState() => _LivePageState();
}

class _LivePageState extends State<LivePage> with SubmitGuard {
  late Future<LiveSession> _future = _load();
  LiveSession? _latest;
  StreamSubscription<LiveSession>? _updates;
  final TextEditingController _danmaku = TextEditingController();
  final String _joinKey = newIdempotencyKey();
  final String _leaveKey = newIdempotencyKey();
  bool _joined = false;

  Future<LiveSession> _load() async {
    final session = widget.host
        ? await widget.ports.live.start(
            widget.channel,
            '直播',
            idempotencyKey: _joinKey,
          )
        : await widget.ports.live.join(
            widget.channel,
            idempotencyKey: _joinKey,
          );
    _joined = true;
    _latest = session;
    _listen();
    return session;
  }

  void _listen() {
    _updates?.cancel();
    _updates = widget.ports.live
        .updates(widget.channel)
        .listen(
          (session) {
            if (mounted) setState(() => _latest = session);
          },
          onError: (Object error) {
            if (!mounted) return;
            final text = error is G13Failure ? error.displayText : '$error';
            ScaffoldMessenger.of(context)
                .showSnackBar(SnackBar(content: Text(text)));
          },
        );
  }

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = _load();
    });
  }

  Future<void> _stop() async {
    await guard(() async {
      try {
        final session = await widget.ports.live.stop(
          widget.channel,
          idempotencyKey: 'stop-${widget.channel}-$_joinKey',
        );
        _joined = false;
        if (!mounted) return;
        setState(() => _latest = session);
      } catch (error) {
        if (!mounted) return;
        final text = error is G13Failure ? error.displayText : '$error';
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(text)));
      }
    });
  }

  Future<void> _sendDanmaku() async {
    final text = _danmaku.text.trim();
    if (text.isEmpty) return;
    try {
      await widget.ports.live.danmaku(
        widget.channel,
        text,
        idempotencyKey:
            'danmaku-${widget.channel}-${text.hashCode}-${DateTime.now().millisecondsSinceEpoch}',
      );
      _danmaku.clear();
    } catch (error) {
      if (!mounted) return;
      final message = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context)
          .showSnackBar(SnackBar(content: Text(message)));
    }
  }

  @override
  void dispose() {
    _updates?.cancel();
    if (_joined) {
      // dispose 里不能 await；退房请求发出即可，服务端按幂等键去重，
      // 连接断开也会让观看位过期。
      unawaited(
        widget.ports.live
            .leave(widget.channel, idempotencyKey: _leaveKey)
            .then<void>((_) {}, onError: (Object _) {}),
      );
    }
    _danmaku.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: _latest?.title ?? '直播',
      subtitle: _latest == null
          ? ''
          : '${_latest!.live ? '直播中' : '已结束'} · ${_latest!.viewers} 人观看',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        if (widget.host)
          TextButton(
            onPressed: submitting || !(_latest?.live ?? false) ? null : _stop,
            child: const Text('结束直播'),
          ),
      ],
      body: AsyncView<LiveSession>(
        future: _future,
        onRetry: _reload,
        builder: (context, session) {
          final current = _latest ?? session;
          return Column(
            children: [
              AspectRatio(
                aspectRatio: 16 / 9,
                child: widget.playerBuilder != null
                    ? widget.playerBuilder!(context, current.hlsUrl)
                    : Container(
                        color: Colors.black87,
                        alignment: Alignment.center,
                        child: Column(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            Icon(
                              current.live
                                  ? Icons.live_tv
                                  : Icons.stop_circle_outlined,
                              color: Colors.white54,
                              size: 36,
                            ),
                            const SizedBox(height: 8),
                            Text(
                              current.live ? '直播画面由平台桥渲染' : '直播已结束',
                              style: const TextStyle(
                                color: Colors.white54,
                                fontSize: 12,
                              ),
                            ),
                          ],
                        ),
                      ),
              ),
              Expanded(
                child: Center(
                  child: Text(
                    current.live ? '弹幕与互动在下方输入' : '主播已结束，本场不再接收弹幕',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                ),
              ),
              SafeArea(
                top: false,
                child: Padding(
                  padding: const EdgeInsets.all(12),
                  child: Row(
                    children: [
                      Expanded(
                        child: TextField(
                          controller: _danmaku,
                          enabled: current.live,
                          decoration: const InputDecoration(
                            hintText: '发一条弹幕',
                            isDense: true,
                            border: OutlineInputBorder(),
                          ),
                          onSubmitted: (_) => _sendDanmaku(),
                        ),
                      ),
                      const SizedBox(width: 8),
                      FilledButton(
                        onPressed: current.live ? _sendDanmaku : null,
                        child: const Text('发送'),
                      ),
                    ],
                  ),
                ),
              ),
            ],
          );
        },
      ),
    );
  }
}
