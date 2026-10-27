/// 移动端产品壳：底部栏、页面栈、系统返回、应用锁与深链。
///
/// 页面不知道自己在第几个 tab，也不知道链接从哪来；路由决策集中在这里，
/// 这样"返回键误退出/误挂断"这类问题只需要在一个地方修。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../platform/platform_ports.dart';
import 'app_lock.dart';
import 'contracts.dart';
import 'deep_link.dart';
import 'theme.dart';

class ShellTabSpec {
  const ShellTabSpec({
    required this.label,
    required this.icon,
    required this.selectedIcon,
    required this.builder,
  });

  final String label;
  final IconData icon;
  final IconData selectedIcon;
  final WidgetBuilder builder;
}

class AppShell extends StatefulWidget {
  const AppShell({
    super.key,
    required this.tabs,
    required this.lock,
    required this.lifecycle,
    required this.onSignOut,
    this.deepLinks,
    this.chatPageBuilder,
    this.qrApprovalPageBuilder,
    this.onResumed,
    this.onPaused,
  }) : assert(tabs.length > 0);

  final List<ShellTabSpec> tabs;
  final AppLockController lock;
  final LifecyclePort lifecycle;
  final Future<void> Function() onSignOut;
  final Stream<Uri>? deepLinks;

  /// 深链打开会话时由上层构造页面；为空表示该入口尚未接线。
  final Widget Function(BuildContext context, String conv)? chatPageBuilder;

  /// 扫码确认页。[QrApprovalTarget.secret] 为空表示只有票号（桌面侧通知带来的
  /// `wxlike://qr/<id>`），那种票扫不了也批不了，页面会如实显示。
  final Widget Function(BuildContext context, QrApprovalTarget target)? qrApprovalPageBuilder;
  final VoidCallback? onResumed;
  final VoidCallback? onPaused;

  @override
  State<AppShell> createState() => AppShellState();
}

class AppShellState extends State<AppShell> {
  int _index = 0;
  StreamSubscription<Uri>? _linkSub;
  StreamSubscription<AppLifecycleSignal>? _lifecycleSub;

  int get currentTab => _index;

  @override
  void initState() {
    super.initState();
    widget.lock.addListener(_rebuild);
    _lifecycleSub = widget.lifecycle.signals.listen(_onLifecycle);
    _linkSub = widget.deepLinks?.listen(handleDeepLink);
  }

  void _rebuild() {
    if (mounted) setState(() {});
  }

  void _onLifecycle(AppLifecycleSignal signal) {
    switch (signal) {
      case AppLifecycleSignal.resumed:
        widget.lock.onForeground();
        widget.onResumed?.call();
      case AppLifecycleSignal.paused:
      case AppLifecycleSignal.detached:
        widget.lock.onBackground();
        widget.onPaused?.call();
      case AppLifecycleSignal.inactive:
        break;
    }
  }

  void selectTab(int index) {
    if (index < 0 || index >= widget.tabs.length || index == _index) return;
    setState(() => _index = index);
  }

  /// 外部（平台深链桥、通知点击）统一从这里进入。
  Future<void> handleDeepLink(Uri uri) async {
    final link = DeepLink.parse(uri);
    if (!mounted) return;
    switch (link.kind) {
      case DeepLinkKind.chat:
        final builder = widget.chatPageBuilder;
        if (builder == null) {
          _toast('会话链接暂不可用：会话语义尚未接线');
          return;
        }
        // 不 await：push 的 Future 在页面关闭时才完成，深链处理不该挂住。
        unawaited(Navigator.of(context).push(
          MaterialPageRoute<void>(builder: (context) => builder(context, link.conv)),
        ));
      case DeepLinkKind.qrApproval:
        final builder = widget.qrApprovalPageBuilder;
        if (builder == null) {
          _toast('扫码确认暂不可用：身份域绑定尚未接线');
          return;
        }
        unawaited(Navigator.of(context).push(
          MaterialPageRoute<void>(
            builder: (context) => builder(
              context,
              QrApprovalTarget(challengeId: link.challengeId),
            ),
          ),
        ));
      case DeepLinkKind.qrScanApproval:
        // 摄像头/系统深链把桌面二维码原文送进来：这一条带 secret，能扫能批。
        final builder = widget.qrApprovalPageBuilder;
        if (builder == null) {
          _toast('扫码确认暂不可用：身份域绑定尚未接线');
          return;
        }
        unawaited(Navigator.of(context).push(
          MaterialPageRoute<void>(
            builder: (context) => builder(
              context,
              QrApprovalTarget(challengeId: link.challengeId, secret: link.secret),
            ),
          ),
        ));
      case DeepLinkKind.contacts:
        selectTab(1);
      case DeepLinkKind.unknown:
        _toast('无法识别的链接：${link.raw}');
    }
  }

  void _toast(String message) {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(message)));
  }

  @override
  void dispose() {
    widget.lock.removeListener(_rebuild);
    _linkSub?.cancel();
    _lifecycleSub?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return PopScope(
      // 根路由永远不能被系统返回键弹出——弹掉就只剩一张白屏。
      // 返回语义集中在这里：非首页回首页，首页才退出应用。
      canPop: false,
      onPopInvokedWithResult: (didPop, _) {
        if (didPop) return;
        if (_index != 0) {
          selectTab(0);
        } else {
          SystemNavigator.pop();
        }
      },
      child: Stack(
        children: [
          Scaffold(
            body: IndexedStack(
              index: _index,
              children: [
                for (final tab in widget.tabs) SafeArea(top: false, child: tab.builder(context)),
              ],
            ),
            bottomNavigationBar: NavigationBar(
              selectedIndex: _index,
              onDestinationSelected: selectTab,
              destinations: [
                for (final tab in widget.tabs)
                  NavigationDestination(
                    icon: Icon(tab.icon),
                    selectedIcon: Icon(tab.selectedIcon),
                    label: tab.label,
                  ),
              ],
            ),
          ),
          if (widget.lock.locked)
            Positioned.fill(
              child: AppLockScreen(lock: widget.lock, onSignOut: widget.onSignOut),
            ),
        ],
      ),
    );
  }
}

class AppLockScreen extends StatefulWidget {
  const AppLockScreen({super.key, required this.lock, required this.onSignOut});

  final AppLockController lock;
  final Future<void> Function() onSignOut;

  @override
  State<AppLockScreen> createState() => _AppLockScreenState();
}

class _AppLockScreenState extends State<AppLockScreen> {
  final _pin = TextEditingController();
  bool _signingOut = false;

  @override
  void dispose() {
    _pin.dispose();
    super.dispose();
  }

  Future<void> _unlock() async {
    final ok = await widget.lock.unlock(_pin.text);
    if (ok) _pin.clear();
    if (mounted) setState(() {});
  }

  Future<void> _signOut() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('退出登录？'),
        content: const Text('退出后需要重新登录才能读取本机缓存的消息。'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('退出')),
        ],
      ),
    );
    if (confirmed != true || _signingOut) return;
    setState(() => _signingOut = true);
    try {
      await widget.onSignOut();
    } finally {
      if (mounted) setState(() => _signingOut = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final dark = Theme.of(context).brightness == Brightness.dark;
    return Material(
      color: dark ? WxColors.darkBg : WxColors.surface,
      child: SafeArea(
        child: Center(
          child: SingleChildScrollView(
            padding: const EdgeInsets.symmetric(horizontal: 32, vertical: 24),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(Icons.lock_outline, size: 44, color: WxColors.brand),
                const SizedBox(height: 16),
                const Text('应用已锁定', style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600)),
                const SizedBox(height: 6),
                Text(
                  '输入设备 PIN 继续',
                  style: TextStyle(fontSize: 13, color: dark ? WxColors.darkTextSecondary : WxColors.textSecondary),
                ),
                const SizedBox(height: 24),
                TextField(
                  key: const Key('app-lock-pin'),
                  controller: _pin,
                  obscureText: true,
                  keyboardType: TextInputType.number,
                  textAlign: TextAlign.center,
                  maxLength: 6,
                  onSubmitted: (_) => _unlock(),
                  decoration: const InputDecoration(
                    counterText: '',
                    border: OutlineInputBorder(),
                    hintText: '4–6 位数字 PIN',
                  ),
                ),
                if (widget.lock.error != null) ...[
                  const SizedBox(height: 8),
                  Text(widget.lock.error!, style: const TextStyle(color: WxColors.danger, fontSize: 12)),
                ],
                const SizedBox(height: 16),
                SizedBox(
                  width: double.infinity,
                  child: FilledButton(
                    onPressed: widget.lock.busy ? null : _unlock,
                    child: widget.lock.busy
                        ? const SizedBox(height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                        : const Text('解锁'),
                  ),
                ),
                const SizedBox(height: 8),
                TextButton(onPressed: _signingOut ? null : _signOut, child: const Text('忘记 PIN，退出登录')),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
