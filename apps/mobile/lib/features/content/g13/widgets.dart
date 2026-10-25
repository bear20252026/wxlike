/// G13 全组共用的小组件：页面壳、加载/空/失败三态、金额显示、提交防重。
///
/// 三态是硬要求：加载中不能显示空列表，失败不能显示成空，未授权不能显示成
/// "没有内容"。所有页面都走这里，避免每个页面各写一套看起来差不多、行为不同
/// 的兜底。
library;

import 'package:flutter/material.dart';

import 'core.dart';
import 'ports.dart';

class G13Scaffold extends StatelessWidget {
  const G13Scaffold({
    super.key,
    required this.title,
    required this.body,
    this.subtitle = '',
    this.actions = const <Widget>[],
    this.floatingActionButton,
    this.onBack,
  });

  final String title;
  final String subtitle;
  final List<Widget> actions;
  final Widget body;
  final Widget? floatingActionButton;
  final VoidCallback? onBack;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        leading: onBack == null
            ? null
            : IconButton(icon: const Icon(Icons.arrow_back), onPressed: onBack),
        title: subtitle.isEmpty
            ? Text(title)
            : Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(title),
                  Text(subtitle, style: Theme.of(context).textTheme.bodySmall),
                ],
              ),
        actions: actions,
      ),
      body: body,
      floatingActionButton: floatingActionButton,
    );
  }
}

/// 页面级三态。`isEmpty` 只在成功拿到数据后才调用。
class AsyncView<T> extends StatelessWidget {
  const AsyncView({
    super.key,
    required this.future,
    required this.builder,
    this.isEmpty,
    this.emptyText = '这里还没有内容',
    this.emptyHint = '',
    this.onRetry,
    this.loadingText = '加载中…',
  });

  final Future<T> future;
  final Widget Function(BuildContext context, T data) builder;
  final bool Function(T data)? isEmpty;
  final String emptyText;
  final String emptyHint;
  final VoidCallback? onRetry;
  final String loadingText;

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<T>(
      future: future,
      builder: (context, snapshot) {
        if (snapshot.connectionState != ConnectionState.done) {
          return _Loading(text: loadingText);
        }
        if (snapshot.hasError) {
          return FailureView(error: snapshot.error!, onRetry: onRetry);
        }
        final data = snapshot.data;
        if (data == null) {
          return const _Loading(text: '加载中…');
        }
        if (isEmpty != null && isEmpty!(data)) {
          return EmptyView(text: emptyText, hint: emptyHint);
        }
        return builder(context, data);
      },
    );
  }
}

class _Loading extends StatelessWidget {
  const _Loading({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const CircularProgressIndicator(),
            const SizedBox(height: 12),
            Text(text, style: Theme.of(context).textTheme.bodySmall),
          ],
        ),
      ),
    );
  }
}

class EmptyView extends StatelessWidget {
  const EmptyView({super.key, required this.text, this.hint = '', this.action});

  final String text;
  final String hint;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final muted = Theme.of(context).colorScheme.onSurfaceVariant;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.inbox_outlined, size: 40, color: muted),
            const SizedBox(height: 12),
            Text(text, style: TextStyle(color: muted)),
            if (hint.isNotEmpty) ...[
              const SizedBox(height: 6),
              Text(
                hint,
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
            if (action != null) ...[const SizedBox(height: 16), action!],
          ],
        ),
      ),
    );
  }
}

/// 失败面板。未授权与"上游未接线"不显示重试按钮：重试不会改变结果，
/// 给一个假按钮只会让用户以为再点一次就能好。
class FailureView extends StatelessWidget {
  const FailureView({super.key, required this.error, this.onRetry});

  final Object error;
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    final failure = error is G13Failure ? error as G13Failure : null;
    final scheme = Theme.of(context).colorScheme;
    final canRetry = onRetry != null && (failure?.retryable ?? true);
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              failure?.kind == G13FailureKind.unavailable
                  ? Icons.link_off
                  : Icons.error_outline,
              size: 40,
              color: failure?.kind == G13FailureKind.unavailable
                  ? scheme.outline
                  : scheme.error,
            ),
            const SizedBox(height: 12),
            Text(
              failure?.displayText ?? '出错了：$error',
              textAlign: TextAlign.center,
            ),
            if (failure?.kind == G13FailureKind.unavailable) ...[
              const SizedBox(height: 8),
              Text(
                '这项功能依赖上游生成绑定，接线后可重试。',
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
            if (canRetry) ...[
              const SizedBox(height: 16),
              FilledButton(onPressed: onRetry, child: const Text('重试')),
            ],
          ],
        ),
      ),
    );
  }
}

/// 列表里已经拿到数据、但翻页或写操作失败时的条幅；不清空已有内容。
class InlineFailure extends StatelessWidget {
  const InlineFailure({super.key, required this.error, this.onRetry});

  final Object error;
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    final failure = error is G13Failure ? error as G13Failure : null;
    final scheme = Theme.of(context).colorScheme;
    return Container(
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: scheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: [
          Expanded(
            child: Text(
              failure?.displayText ?? '$error',
              style: TextStyle(color: scheme.onErrorContainer),
            ),
          ),
          if (onRetry != null && (failure?.retryable ?? true))
            TextButton(onPressed: onRetry, child: const Text('重试')),
        ],
      ),
    );
  }
}

/// 金额显示必须走端口里的 Rust 格式化；拿不到时显示破折号，不自己除以 100。
class MoneyText extends StatelessWidget {
  const MoneyText({
    super.key,
    required this.money,
    required this.format,
    this.style,
    this.placeholder = '—',
  });

  final Money? money;
  final FormatPort format;
  final TextStyle? style;
  final String placeholder;

  @override
  Widget build(BuildContext context) {
    final value = money;
    if (value == null) return Text(placeholder, style: style);
    String text;
    try {
      text = format.money(value);
    } catch (_) {
      text = placeholder;
    }
    return Text(text, style: style);
  }
}

class SectionTitle extends StatelessWidget {
  const SectionTitle({super.key, required this.text, this.trailing});

  final String text;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 16, 16, 6),
      child: Row(
        children: [
          Expanded(
            child: Text(
              text,
              style: Theme.of(context).textTheme.labelLarge?.copyWith(
                color: Theme.of(context).colorScheme.onSurfaceVariant,
              ),
            ),
          ),
          ?trailing,
        ],
      ),
    );
  }
}

class SettingsTile extends StatelessWidget {
  const SettingsTile({
    super.key,
    required this.title,
    this.subtitle = '',
    this.icon,
    this.trailing,
    this.onTap,
    this.enabled = true,
  });

  final String title;
  final String subtitle;
  final IconData? icon;
  final Widget? trailing;
  final VoidCallback? onTap;
  final bool enabled;

  @override
  Widget build(BuildContext context) {
    return ListTile(
      enabled: enabled,
      leading: icon == null ? null : Icon(icon),
      title: Text(title),
      subtitle: subtitle.isEmpty ? null : Text(subtitle),
      trailing:
          trailing ?? (onTap == null ? null : const Icon(Icons.chevron_right)),
      onTap: onTap,
    );
  }
}

class Pill extends StatelessWidget {
  const Pill({super.key, required this.text, this.color});

  final String text;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final tint = color ?? scheme.primary;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        color: tint.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(text, style: TextStyle(fontSize: 11, color: tint)),
    );
  }
}

class AvatarDot extends StatelessWidget {
  const AvatarDot({
    super.key,
    required this.label,
    this.color = const Color(0xff07c160),
  });

  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) {
    final initial = label.isEmpty
        ? '?'
        : String.fromCharCode(label.runes.first);
    return CircleAvatar(
      backgroundColor: color.withValues(alpha: 0.16),
      foregroundColor: color,
      child: Text(initial),
    );
  }
}

/// 写操作防重：同一个页面在提交完成前忽略后续点击。
///
/// 幂等键仍然由页面持有并在重试时复用——防重解决"手抖连点"，
/// 幂等键解决"请求发出去了但没看到结果"。
mixin SubmitGuard<T extends StatefulWidget> on State<T> {
  bool _submitting = false;

  bool get submitting => _submitting;

  Future<bool> guard(Future<void> Function() action) async {
    if (_submitting) return false;
    setState(() => _submitting = true);
    try {
      await action();
      return true;
    } finally {
      if (mounted) setState(() => _submitting = false);
    }
  }
}

/// 时间显示只做"本地时区呈现"，不做业务上的过期判断——过期由服务端字段决定。
String formatTime(DateTime value) {
  String two(int v) => v.toString().padLeft(2, '0');
  final local = value.toLocal();
  return '${local.year}-${two(local.month)}-${two(local.day)} '
      '${two(local.hour)}:${two(local.minute)}';
}

String formatBytes(int bytes) {
  if (bytes < 1024) return '$bytes B';
  if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)} KB';
  if (bytes < 1024 * 1024 * 1024) {
    return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
  }
  return '${(bytes / (1024 * 1024 * 1024)).toStringAsFixed(2)} GB';
}
