/// G13-03：卡包（会员卡、券、票证）。
///
/// 凭券核销必须带商户码里解析出的 merchant token；没有 token 时不提供
/// "假装核销"的按钮。状态更新只认服务端返回，一次性转换失败就显示失败。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

class CardsPage extends StatefulWidget {
  const CardsPage({super.key, required this.ports, this.merchantToken = ''});

  final G13Ports ports;

  /// 扫描商户码后得到的核销令牌；为空表示当前不是核销场景。
  final String merchantToken;

  @override
  State<CardsPage> createState() => _CardsPageState();
}

class _CardsPageState extends State<CardsPage> {
  late Future<PageResult<CardEntry>> _future = widget.ports.commerce
      .entitlements();
  String? _busyId;
  String? _error;

  void _reload() {
    if (!mounted) return;
    setState(() {
      _error = null;
      _future = widget.ports.commerce.entitlements();
    });
  }

  Future<void> _redeem(CardEntry entry) async {
    if (widget.merchantToken.isEmpty) {
      setState(() => _error = '请在商户处扫码后再核销');
      return;
    }
    setState(() {
      _busyId = entry.id;
      _error = null;
    });
    try {
      await widget.ports.commerce.redeem(
        entry.id,
        merchantToken: widget.merchantToken,
        idempotencyKey: 'redeem-${entry.id}-${widget.merchantToken.hashCode}',
      );
      _reload();
    } catch (error) {
      if (!mounted) return;
      setState(
        () => _error = error is G13Failure ? error.displayText : '$error',
      );
    } finally {
      if (mounted) setState(() => _busyId = null);
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '卡包',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
      ],
      body: Column(
        children: [
          if (_error != null)
            InlineFailure(
              error: G13Failure(G13FailureKind.unexpected, 'CARDS', _error!),
            ),
          Expanded(
            child: AsyncView<PageResult<CardEntry>>(
              future: _future,
              onRetry: _reload,
              isEmpty: (page) => page.isEmpty,
              emptyText: '卡包是空的',
              emptyHint: '领取的会员卡、券和票证会出现在这里',
              builder: (context, page) => ListView.separated(
                itemCount: page.items.length,
                separatorBuilder: (_, _) => const Divider(height: 1),
                itemBuilder: (context, index) {
                  final entry = page.items[index];
                  return ListTile(
                    leading: Icon(switch (entry.kind) {
                      'member' => Icons.badge_outlined,
                      'coupon' => Icons.confirmation_number_outlined,
                      _ => Icons.local_activity_outlined,
                    }),
                    title: Text(entry.title),
                    subtitle: Text(
                      '${entry.subtitle} · 有效期至 ${formatTime(entry.validUntil)}',
                    ),
                    trailing: _busyId == entry.id
                        ? const SizedBox(
                            width: 18,
                            height: 18,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : TextButton(
                            onPressed: entry.usable
                                ? () => _redeem(entry)
                                : null,
                            child: Text(entry.usable ? '核销' : '不可用'),
                          ),
                  );
                },
              ),
            ),
          ),
        ],
      ),
    );
  }
}
