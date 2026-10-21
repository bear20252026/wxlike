import 'package:flutter/material.dart';

import 'service.dart';

/// 三屏共用的错误条。界面只决定"怎么说"，"是哪一类"由 Rust 判（`describeError`）。
class ErrorBox extends StatelessWidget {
  const ErrorBox({super.key, required this.message});

  final String message;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: scheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Text(message, style: TextStyle(color: scheme.onErrorContainer)),
    );
  }
}

/// 顶部的连接状态。`state` 与 `pending` 都来自 Rust，这里只着色。
class LiveChip extends StatelessWidget {
  const LiveChip({super.key, required this.info});

  final LiveInfo info;

  @override
  Widget build(BuildContext context) {
    final color = switch (info.state) {
      LiveState.online => const Color(0xff07c160),
      LiveState.offline => Colors.orange,
      LiveState.signedOut => Colors.blueGrey,
    };
    final label = switch (info.state) {
      LiveState.online => '在线',
      LiveState.offline => '离线',
      LiveState.signedOut => '未登录',
    };
    final pending = info.pending > 0 ? ' · ${info.pending} 待发送' : '';
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(Icons.circle, size: 10, color: color),
        const SizedBox(width: 6),
        Text('$label$pending'),
      ],
    );
  }
}
