/// G13-05：设置。
///
/// 每个开关都写 `preferences.update`，成功后以服务端返回为准；失败保留旧值并
/// 显示原因，绝不本地"看起来生效"。退出登录由组合根注入登录服务动作，未注入
/// 时明确说明阻塞，不做假跳转。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';
import 'about_page.dart';
import 'account_security_page.dart';
import 'profile_page.dart';
import 'storage_page.dart';

/// 退出登录动作（同时让设备 token 与推送失效）；由组合根提供。
typedef LogoutAction = Future<void> Function();

const List<(String, String)> g13Languages = [
  ('zh-Hans', '简体中文'),
  ('zh-Hant', '繁體中文'),
  ('en', 'English'),
  ('ja', '日本語'),
  ('ko', '한국어'),
];

String _languageLabel(String code) {
  for (final (value, label) in g13Languages) {
    if (value == code) return label;
  }
  return code;
}

class SettingsPage extends StatefulWidget {
  const SettingsPage({
    super.key,
    required this.ports,
    this.logout,
    this.openExternal,
    this.agreements = g13Agreements,
    this.appVersion = g13AppVersion,
  });

  final G13Ports ports;
  final LogoutAction? logout;
  final ExternalLinkOpener? openExternal;
  final List<AgreementEntry> agreements;
  final String appVersion;

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  late Future<Preferences> _future = _load();
  Preferences? _prefs;
  bool _saving = false;
  bool _loggingOut = false;
  Object? _error;
  Preferences? _pending;
  String _pendingKey = '';

  Future<Preferences> _load() async {
    final prefs = await widget.ports.preferences.load();
    if (mounted) setState(() => _prefs = prefs);
    return prefs;
  }

  void _reload() {
    if (!mounted) return;
    setState(() {
      _error = null;
      _pending = null;
      _pendingKey = '';
      _future = _load();
    });
  }

  Future<void> _send(Preferences next, String key) async {
    try {
      final saved = await widget.ports.preferences.update(
        next,
        idempotencyKey: key,
      );
      if (!mounted) return;
      setState(() {
        _prefs = saved;
        _pending = null;
        _pendingKey = '';
        _error = null;
      });
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  Future<void> _apply(Preferences next) async {
    if (_saving) return;
    final key = newIdempotencyKey();
    setState(() {
      _saving = true;
      _pending = next;
      _pendingKey = key;
      _error = null;
    });
    await _send(next, key);
  }

  Future<void> _retry() async {
    final next = _pending;
    final key = _pendingKey;
    if (_saving || next == null || key.isEmpty) return;
    setState(() {
      _saving = true;
      _error = null;
    });
    await _send(next, key);
  }

  Future<void> _pickLanguage(Preferences prefs) async {
    final code = await showDialog<String>(
      context: context,
      builder: (context) => SimpleDialog(
        title: const Text('语言'),
        children: [
          for (final (value, label) in g13Languages)
            ListTile(
              title: Text(label),
              subtitle: Text(value),
              trailing: prefs.language == value
                  ? const Icon(Icons.check)
                  : null,
              onTap: () => Navigator.of(context).pop(value),
            ),
        ],
      ),
    );
    if (code == null || code == prefs.language || !mounted) return;
    await _apply(prefs.copyWith(language: code));
  }

  Future<void> _logout() async {
    if (_saving || _loggingOut) return;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('退出登录'),
        content: const Text('退出后本机登录态清除，设备 token 与推送将失效。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('退出'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    final action = widget.logout;
    if (action == null) {
      setState(
        () => _error = const G13Failure(
          G13FailureKind.unavailable,
          'LOGOUT_NOT_WIRED',
          '退出登录需要组合根注入登录服务动作',
        ),
      );
      return;
    }
    setState(() {
      _loggingOut = true;
      _error = null;
    });
    try {
      await action();
      if (!mounted) return;
      Navigator.of(context).maybePop();
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _loggingOut = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final prefs = _prefs;
    return G13Scaffold(
      title: '设置',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(
          onPressed: _saving ? null : _reload,
          icon: const Icon(Icons.refresh),
        ),
      ],
      body: prefs == null
          ? AsyncView<Preferences>(
              future: _future,
              onRetry: _reload,
              builder: (context, loaded) => _settings(loaded),
            )
          : _settings(prefs),
    );
  }

  Widget _settings(Preferences prefs) {
    final error = _error;
    return ListView(
      children: [
        if (error != null)
          InlineFailure(
            error: error,
            onRetry: error is G13Failure && error.retryable ? _retry : _reload,
          ),
        if (_saving) const LinearProgressIndicator(),
        const SectionTitle(text: '账号'),
        SettingsTile(
          icon: Icons.person_outline,
          title: '我的资料',
          onTap: () => Navigator.of(context).push(
            MaterialPageRoute<void>(
              builder: (_) => ProfilePage(ports: widget.ports),
            ),
          ),
        ),
        SettingsTile(
          icon: Icons.shield_outlined,
          title: '账号与安全',
          onTap: () => Navigator.of(context).push(
            MaterialPageRoute<void>(
              builder: (_) => AccountSecurityPage(ports: widget.ports),
            ),
          ),
        ),
        const Divider(height: 1),
        const SectionTitle(text: '通知'),
        _toggle(
          title: '新消息通知',
          subtitle: '关闭后不再接收消息推送',
          icon: Icons.notifications_outlined,
          value: prefs.messageNotifications,
          make: (value) => prefs.copyWith(messageNotifications: value),
        ),
        _toggle(
          title: '声音',
          icon: Icons.volume_up_outlined,
          value: prefs.sound,
          make: (value) => prefs.copyWith(sound: value),
        ),
        _toggle(
          title: '振动',
          icon: Icons.vibration,
          value: prefs.vibration,
          make: (value) => prefs.copyWith(vibration: value),
        ),
        const Divider(height: 1),
        const SectionTitle(text: '隐私'),
        _toggle(
          title: '通过手机号找到我',
          icon: Icons.phone_iphone,
          value: prefs.findMeByPhone,
          make: (value) => prefs.copyWith(findMeByPhone: value),
        ),
        _toggle(
          title: '钱包锁',
          subtitle: '进入钱包前需要验证',
          icon: Icons.lock_outline,
          value: prefs.walletLock,
          make: (value) => prefs.copyWith(walletLock: value),
        ),
        _toggle(
          title: '青少年模式',
          icon: Icons.child_care_outlined,
          value: prefs.youthMode,
          make: (value) => prefs.copyWith(youthMode: value),
        ),
        const Divider(height: 1),
        const SectionTitle(text: '通用'),
        _toggle(
          title: '深色模式',
          subtitle: '保存后由根布局应用主题',
          icon: Icons.dark_mode_outlined,
          value: prefs.darkMode,
          make: (value) => prefs.copyWith(darkMode: value),
        ),
        SettingsTile(
          icon: Icons.translate,
          title: '语言',
          subtitle: _languageLabel(prefs.language),
          enabled: !_saving,
          onTap: () => _pickLanguage(prefs),
        ),
        const Divider(height: 1),
        const SectionTitle(text: '存储'),
        SettingsTile(
          icon: Icons.storage_outlined,
          title: '存储空间',
          onTap: () => Navigator.of(context).push(
            MaterialPageRoute<void>(
              builder: (_) => StoragePage(ports: widget.ports),
            ),
          ),
        ),
        SettingsTile(
          icon: Icons.help_outline,
          title: '帮助与关于',
          onTap: () => Navigator.of(context).push(
            MaterialPageRoute<void>(
              builder: (_) => AboutPage(
                openExternal: widget.openExternal,
                agreements: widget.agreements,
                appVersion: widget.appVersion,
              ),
            ),
          ),
        ),
        const SizedBox(height: 24),
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16),
          child: OutlinedButton(
            onPressed: _loggingOut ? null : _logout,
            child: _loggingOut
                ? const SizedBox(
                    width: 18,
                    height: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Text('退出登录'),
          ),
        ),
        const SizedBox(height: 32),
      ],
    );
  }

  Widget _toggle({
    required String title,
    String subtitle = '',
    required IconData icon,
    required bool value,
    required Preferences Function(bool value) make,
  }) {
    return SwitchListTile(
      secondary: Icon(icon),
      title: Text(title),
      subtitle: subtitle.isEmpty ? null : Text(subtitle),
      value: value,
      onChanged: _saving
          ? null
          : (next) {
              _apply(make(next));
            },
    );
  }
}
