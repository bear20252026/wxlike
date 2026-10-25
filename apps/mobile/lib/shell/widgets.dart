import 'package:flutter/material.dart';

import 'contracts.dart';
import 'theme.dart';

class G12ErrorBanner extends StatelessWidget {
  const G12ErrorBanner({super.key, required this.message, this.onRetry});

  final String message;
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.fromLTRB(12, 10, 8, 10),
      decoration: BoxDecoration(
        color: scheme.errorContainer,
        borderRadius: BorderRadius.circular(WxRadius.tile),
      ),
      child: Row(
        children: [
          Expanded(
            child: Text(
              message,
              style: TextStyle(color: scheme.onErrorContainer, fontSize: 13),
            ),
          ),
          if (onRetry != null)
            TextButton(
              onPressed: onRetry,
              child: const Text('重试'),
            ),
        ],
      ),
    );
  }
}

class G12EmptyState extends StatelessWidget {
  const G12EmptyState({super.key, required this.title, this.hint, this.action});

  final String title;
  final String? hint;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 32, vertical: 56),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.inbox_outlined, size: 40, color: colors.outline),
          const SizedBox(height: 12),
          Text(title, textAlign: TextAlign.center),
          if (hint != null) ...[
            const SizedBox(height: 6),
            Text(
              hint!,
              textAlign: TextAlign.center,
              style: TextStyle(fontSize: 12, color: colors.outline),
            ),
          ],
          if (action != null) ...[const SizedBox(height: 16), action!],
        ],
      ),
    );
  }
}

/// 上游未接线时的统一状态：说明是哪块契约没到，而不是显示空列表。
class G12BlockedState extends StatelessWidget {
  const G12BlockedState({
    super.key,
    required this.title,
    required this.reason,
    this.onRetry,
  });

  final String title;
  final String reason;
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    return G12EmptyState(
      title: title,
      hint: reason,
      action: onRetry == null
          ? null
          : OutlinedButton(onPressed: onRetry, child: const Text('重新检查')),
    );
  }
}

String twoDigits(int value) => value.toString().padLeft(2, '0');

String formatCountdown(Duration duration) {
  final seconds = duration.inSeconds;
  if (seconds >= 60) return '${seconds ~/ 60}:${twoDigits(seconds % 60)}';
  return '${seconds}s';
}

String formatClockTime(DateTime utc) {
  final local = utc.toLocal();
  return '${twoDigits(local.hour)}:${twoDigits(local.minute)}';
}

String formatMessageTime(int sentAtMs) {
  final time = DateTime.fromMillisecondsSinceEpoch(sentAtMs);
  final now = DateTime.now();
  final sameDay = time.year == now.year && time.month == now.month && time.day == now.day;
  if (sameDay) return formatClockTime(time.toUtc());
  return '${time.month}/${time.day} ${twoDigits(time.hour)}:${twoDigits(time.minute)}';
}

String failureText(Object error) => asG12Failure(error).displayText;
