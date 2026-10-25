/// G13-02：语音/视频通话。
///
/// 状态全部来自 `call.*` 与服务端事件：接听、静音、扬声器、蓝牙、翻转摄像头、
/// 最小化、挂断都不是本地开关。挂断或对端结束后调用 `releaseMedia`，
/// 摄像头/麦克风指示必须跟着熄灭；最小化只收起界面，不释放媒体。
library;

import 'dart:async';

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';
import '../video/channels_page.dart' show MediaSurfaceBuilder;

/// 从聊天页发起通话。返回值在通话结束/收起后给出，调用方据此刷新消息流。
Future<void> startCall(
  BuildContext context,
  G13Ports ports, {
  required String convId,
  required String peerName,
  required bool video,
  MediaSurfaceBuilder? playerBuilder,
}) async {
  final messenger = ScaffoldMessenger.of(context);
  try {
    final snapshot = await ports.calls.start(
      convId,
      video: video,
      idempotencyKey: 'call-start-$convId-$video',
    );
    if (!context.mounted) return;
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => CallPage(
          ports: ports,
          snapshot: snapshot,
          playerBuilder: playerBuilder,
        ),
      ),
    );
  } catch (error) {
    final text = error is G13Failure ? error.displayText : '$error';
    messenger.showSnackBar(SnackBar(content: Text(text)));
  }
}

class CallPage extends StatefulWidget {
  const CallPage({
    super.key,
    required this.ports,
    required this.snapshot,
    this.playerBuilder,
  });

  final G13Ports ports;
  final CallSnapshot snapshot;
  final MediaSurfaceBuilder? playerBuilder;

  @override
  State<CallPage> createState() => _CallPageState();
}

class _CallPageState extends State<CallPage> {
  late CallSnapshot _call = widget.snapshot;
  StreamSubscription<CallSnapshot>? _updates;
  Timer? _ticker;
  bool _minimizedOnPop = false;
  bool _hangingUp = false;

  @override
  void initState() {
    super.initState();
    _updates = widget.ports.calls.updates().listen(
      _apply,
      onError: (Object error) {
        if (!mounted) return;
        final text = error is G13Failure ? error.displayText : '$error';
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(text)));
      },
    );
    _ticker = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted) setState(() {});
    });
  }

  void _apply(CallSnapshot snapshot) {
    if (!mounted) return;
    setState(() => _call = snapshot);
    if (snapshot.phase == CallPhase.ended) {
      _stopTicker();
      unawaited(
        widget.ports.calls.releaseMedia().then<void>(
          (_) {},
          onError: (Object _) {},
        ),
      );
    }
  }

  void _stopTicker() {
    _ticker?.cancel();
    _ticker = null;
  }

  Future<void> _act(Future<CallSnapshot> Function() action) async {
    try {
      final snapshot = await action();
      if (mounted) setState(() => _call = snapshot);
    } catch (error) {
      if (!mounted) return;
      final text = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    }
  }

  Future<void> _hangUp() async {
    if (_hangingUp || _call.phase == CallPhase.ended) return;
    setState(() => _hangingUp = true);
    try {
      await _act(
        () => widget.ports.calls.hangUp(
          _call.callId,
          idempotencyKey: 'call-hangup-${_call.callId}',
        ),
      );
      await widget.ports.calls.releaseMedia().then<void>(
        (_) {},
        onError: (Object _) {},
      );
      _stopTicker();
    } finally {
      if (mounted) setState(() => _hangingUp = false);
    }
  }

  Future<void> _minimize() async {
    _minimizedOnPop = true;
    await _act(() => widget.ports.calls.setMinimized(_call.callId, true));
    if (mounted) Navigator.of(context).maybePop();
  }

  @override
  void dispose() {
    _stopTicker();
    _updates?.cancel();
    if (!_minimizedOnPop && _call.phase != CallPhase.ended) {
      // 页面被系统销毁但没有走挂断：媒体必须释放，宁可对端看到中断，
      // 也不能让摄像头/麦克风在后台继续工作。
      unawaited(
        widget.ports.calls.releaseMedia().then<void>(
          (_) {},
          onError: (Object _) {},
        ),
      );
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final ended = _call.phase == CallPhase.ended;
    return PopScope(
      canPop: true,
      onPopInvokedWithResult: (didPop, _) {
        if (didPop && !ended && !_minimizedOnPop) {
          _minimizedOnPop = true;
          unawaited(
            widget.ports.calls
                .setMinimized(_call.callId, true)
                .then<void>((_) {}, onError: (Object _) {}),
          );
        }
      },
      child: Scaffold(
        backgroundColor: const Color(0xff101512),
        body: SafeArea(
          child: Column(
            children: [
              const Spacer(),
              AvatarDot(label: _call.peerName, color: const Color(0xff07c160)),
              const SizedBox(height: 12),
              Text(
                _call.peerName,
                style: const TextStyle(color: Colors.white, fontSize: 20),
              ),
              const SizedBox(height: 6),
              Text(
                _statusText(ended),
                style: const TextStyle(color: Colors.white70),
              ),
              if (_call.video && !ended) ...[
                const SizedBox(height: 16),
                ClipRRect(
                  borderRadius: BorderRadius.circular(12),
                  child: SizedBox(
                    height: 180,
                    width: 140,
                    child: _call.cameraOn
                        ? (widget.playerBuilder != null
                              ? widget.playerBuilder!(
                                  context,
                                  'call://${_call.callId}/local',
                                )
                              : Container(
                                  color: Colors.black45,
                                  alignment: Alignment.center,
                                  child: const Text(
                                    '摄像头画面\n由平台桥渲染',
                                    textAlign: TextAlign.center,
                                    style: TextStyle(
                                      color: Colors.white38,
                                      fontSize: 12,
                                    ),
                                  ),
                                ))
                        : Container(
                            color: Colors.black45,
                            alignment: Alignment.center,
                            child: const Icon(
                              Icons.videocam_off,
                              color: Colors.white38,
                            ),
                          ),
                  ),
                ),
              ],
              const Spacer(),
              if (!ended) _controls() else _endedActions(),
              const SizedBox(height: 24),
            ],
          ),
        ),
      ),
    );
  }

  String _statusText(bool ended) {
    if (ended) {
      return _call.endedReason.isEmpty ? '通话已结束' : '通话结束：${_call.endedReason}';
    }
    return switch (_call.phase) {
      CallPhase.dialing => '正在呼叫…',
      CallPhase.ringing => '等待对方接听…',
      CallPhase.connecting => '正在连接…',
      CallPhase.active => _elapsed(),
      _ => '准备中…',
    };
  }

  String _elapsed() {
    final seconds = DateTime.now().difference(_call.startedAt).inSeconds;
    final safe = seconds < 0 ? 0 : seconds;
    final m = (safe ~/ 60).toString().padLeft(2, '0');
    final s = (safe % 60).toString().padLeft(2, '0');
    return '$m:$s';
  }

  Widget _controls() {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 24),
      child: Column(
        children: [
          Row(
            mainAxisAlignment: MainAxisAlignment.spaceEvenly,
            children: [
              _RoundControl(
                icon: _call.muted ? Icons.mic_off : Icons.mic,
                label: _call.muted ? '已静音' : '静音',
                on: _call.muted,
                onTap: () => _act(
                  () => widget.ports.calls.setMuted(
                    _call.callId,
                    !_call.muted,
                    idempotencyKey: 'call-mute-${_call.callId}-${!_call.muted}',
                  ),
                ),
              ),
              _RoundControl(
                icon: _call.speakerOn ? Icons.volume_up : Icons.volume_down,
                label: _call.speakerOn ? '扬声器' : '听筒',
                on: _call.speakerOn,
                onTap: () => _act(
                  () => widget.ports.calls.setSpeaker(
                    _call.callId,
                    !_call.speakerOn,
                    idempotencyKey:
                        'call-speaker-${_call.callId}-${!_call.speakerOn}',
                  ),
                ),
              ),
              if (_call.video)
                _RoundControl(
                  icon: _call.cameraOn ? Icons.videocam : Icons.videocam_off,
                  label: _call.cameraOn ? '关摄像头' : '开摄像头',
                  on: !_call.cameraOn,
                  onTap: () => _act(
                    () => widget.ports.calls.setCamera(
                      _call.callId,
                      !_call.cameraOn,
                      idempotencyKey:
                          'call-camera-${_call.callId}-${!_call.cameraOn}',
                    ),
                  ),
                ),
              if (_call.video)
                _RoundControl(
                  icon: Icons.cameraswitch_outlined,
                  label: _call.frontCamera ? '前置' : '后置',
                  on: false,
                  onTap: () => _act(
                    () => widget.ports.calls.flipCamera(
                      _call.callId,
                      idempotencyKey:
                          'call-flip-${_call.callId}-${!_call.frontCamera}',
                    ),
                  ),
                ),
              _RoundControl(
                icon: _call.bluetoothOn
                    ? Icons.bluetooth
                    : Icons.bluetooth_disabled,
                label: '蓝牙',
                on: _call.bluetoothOn,
                onTap: () => _act(
                  () => widget.ports.calls.setBluetooth(
                    _call.callId,
                    !_call.bluetoothOn,
                    idempotencyKey:
                        'call-bt-${_call.callId}-${!_call.bluetoothOn}',
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: 28),
          Row(
            mainAxisAlignment: MainAxisAlignment.spaceEvenly,
            children: [
              _RoundControl(
                icon: Icons.minimize,
                label: '最小化',
                on: false,
                onTap: _minimize,
              ),
              _RoundControl(
                icon: Icons.call_end,
                label: '挂断',
                on: false,
                danger: true,
                onTap: _hangingUp ? null : _hangUp,
              ),
            ],
          ),
        ],
      ),
    );
  }

  Widget _endedActions() {
    return FilledButton(
      onPressed: () => Navigator.of(context).maybePop(),
      child: const Text('关闭'),
    );
  }
}

class _RoundControl extends StatelessWidget {
  const _RoundControl({
    required this.icon,
    required this.label,
    required this.on,
    this.onTap,
    this.danger = false,
  });

  final IconData icon;
  final String label;
  final bool on;
  final VoidCallback? onTap;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    final background = danger
        ? const Color(0xffe5484d)
        : on
        ? Colors.white.withValues(alpha: 0.9)
        : Colors.white.withValues(alpha: 0.12);
    final foreground = danger || !on ? Colors.white : Colors.black87;
    return Column(
      children: [
        InkResponse(
          onTap: onTap,
          child: Container(
            width: 54,
            height: 54,
            decoration: BoxDecoration(
              color: background,
              shape: BoxShape.circle,
            ),
            child: Icon(icon, color: foreground),
          ),
        ),
        const SizedBox(height: 6),
        Text(
          label,
          style: const TextStyle(color: Colors.white70, fontSize: 12),
        ),
      ],
    );
  }
}

/// 来电页：只有"接听"和"拒绝"两个真实动作；超时与撤销由服务端事件驱动。
class IncomingCallPage extends StatelessWidget {
  const IncomingCallPage({
    super.key,
    required this.ports,
    required this.snapshot,
    this.playerBuilder,
  });

  final G13Ports ports;
  final CallSnapshot snapshot;
  final MediaSurfaceBuilder? playerBuilder;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: const Color(0xff101512),
      body: SafeArea(
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            const Spacer(),
            AvatarDot(label: snapshot.peerName),
            const SizedBox(height: 12),
            Text(
              snapshot.peerName,
              style: const TextStyle(color: Colors.white, fontSize: 20),
            ),
            const SizedBox(height: 6),
            Text(
              snapshot.video ? '邀请你视频通话' : '邀请你语音通话',
              style: const TextStyle(color: Colors.white70),
            ),
            const Spacer(),
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceEvenly,
              children: [
                _RoundControl(
                  icon: Icons.call_end,
                  label: '拒绝',
                  on: false,
                  danger: true,
                  onTap: () async {
                    try {
                      await ports.calls.hangUp(
                        snapshot.callId,
                        idempotencyKey: 'call-decline-${snapshot.callId}',
                      );
                    } catch (_) {
                      // 拒绝失败也要关闭来电界面：服务端超时会撤销，
                      // 界面留在这里反而更危险（看着像还在响）。
                    }
                    if (context.mounted) Navigator.of(context).maybePop();
                  },
                ),
                _RoundControl(
                  icon: snapshot.video ? Icons.videocam : Icons.call,
                  label: '接听',
                  on: true,
                  onTap: () async {
                    try {
                      final answered = await ports.calls.answer(
                        snapshot.callId,
                        idempotencyKey: 'call-answer-${snapshot.callId}',
                      );
                      if (!context.mounted) return;
                      Navigator.of(context).pushReplacement(
                        MaterialPageRoute<void>(
                          builder: (_) => CallPage(
                            ports: ports,
                            snapshot: answered,
                            playerBuilder: playerBuilder,
                          ),
                        ),
                      );
                    } catch (error) {
                      if (!context.mounted) return;
                      final text = error is G13Failure
                          ? error.displayText
                          : '$error';
                      ScaffoldMessenger.of(context)
                          .showSnackBar(SnackBar(content: Text(text)));
                    }
                  },
                ),
              ],
            ),
            const SizedBox(height: 48),
          ],
        ),
      ),
    );
  }
}
