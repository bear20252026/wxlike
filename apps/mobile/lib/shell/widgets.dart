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

/// 服务端状态条：把路线契约里的状态名（CREATED / SCANNED / APPROVED /
/// REJECTED / EXPIRED）原样显示出来，配一句人话。
///
/// 存在的理由就是反对"成功提示"：状态推进必须体现在这块常驻的条上，
/// 而不是闪过一条 toast——toast 会消失，用户无法核对"到底批准成功了没有"。
class G12StateChip extends StatelessWidget {
  const G12StateChip({
    super.key,
    required this.state,
    required this.label,
    this.fatal = false,
  });

  final String state;
  final String label;

  /// 终态（已拒绝/已过期/不能再做决定）用警示色，进行中用品牌色。
  final bool fatal;

  @override
  Widget build(BuildContext context) {
    final color = fatal ? WxColors.danger : WxColors.brand;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.10),
        borderRadius: BorderRadius.circular(WxRadius.tile),
        border: Border.all(color: color.withValues(alpha: 0.35)),
      ),
      child: Row(
        children: [
          Icon(fatal ? Icons.error_outline : Icons.check_circle_outline, size: 16, color: color),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              label,
              style: TextStyle(fontSize: 12, color: color, fontWeight: FontWeight.w600),
            ),
          ),
          if (state.isNotEmpty)
            // 契约状态名跟着条子出现，测试与用户看到的都是同一个词。
            Text(state, style: TextStyle(fontSize: 11, color: color)),
        ],
      ),
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
