/// G13-03：收付款。
///
/// 付款码/收款码的载荷由 `commerce.*` 签发，码图由平台桥按标准 QR 渲染。
/// 页面不自己画格子——画出来的假码扫不出，比没有更糟。保存到相册同样是
/// 真实写操作，失败必须显示出来。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';
import 'wallet_page.dart' show BillsPage;

class PayPage extends StatefulWidget {
  const PayPage({super.key, required this.ports, this.qrBuilder});

  final G13Ports ports;

  /// 平台桥提供的标准二维码渲染器。没有时页面明确说明未接线。
  final Widget Function(BuildContext context, String payload)? qrBuilder;

  @override
  State<PayPage> createState() => _PayPageState();
}

class _PayPageState extends State<PayPage> {
  String _kind = 'pay';
  late Future<String> _code = _load();

  Future<String> _load() => widget.ports.commerce.paymentCode(
    kind: _kind,
    idempotencyKey:
        'code-$_kind-${DateTime.now().millisecondsSinceEpoch ~/ 60000}',
  );

  void _reload() {
    if (!mounted) return;
    setState(() {
      _code = _load();
    });
  }

  Future<void> _save(String payload) async {
    try {
      await widget.ports.commerce.saveQrToAlbum(
        payload,
        idempotencyKey: 'save-code-$_kind-${payload.hashCode}',
      );
      if (!mounted) return;
      ScaffoldMessenger.of(context)
          .showSnackBar(const SnackBar(content: Text('已保存到相册')));
    } catch (error) {
      if (!mounted) return;
      final text = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '收付款',
      onBack: () => Navigator.of(context).maybePop(),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          SegmentedButton<String>(
            segments: const [
              ButtonSegment(value: 'pay', label: Text('付款码')),
              ButtonSegment(value: 'receive', label: Text('收款码')),
            ],
            selected: {_kind},
            onSelectionChanged: (value) {
              setState(() {
                _kind = value.first;
                _code = _load();
              });
            },
          ),
          const SizedBox(height: 16),
          AsyncView<String>(
            future: _code,
            onRetry: _reload,
            builder: (context, payload) => Column(
              children: [
                Container(
                  height: 240,
                  width: 240,
                  decoration: BoxDecoration(
                    border: Border.all(
                      color: Theme.of(context).colorScheme.outlineVariant,
                    ),
                    borderRadius: BorderRadius.circular(12),
                  ),
                  child: widget.qrBuilder != null
                      ? widget.qrBuilder!(context, payload)
                      : const Center(
                          child: Column(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              Icon(Icons.qr_code_2, size: 48),
                              SizedBox(height: 8),
                              Text(
                                '二维码渲染未接入\n需要 G12-05 平台桥',
                                textAlign: TextAlign.center,
                              ),
                            ],
                          ),
                        ),
                ),
                const SizedBox(height: 12),
                SelectableText(
                  payload,
                  style: Theme.of(context).textTheme.bodySmall,
                  textAlign: TextAlign.center,
                ),
                const SizedBox(height: 16),
                Row(
                  mainAxisAlignment: MainAxisAlignment.center,
                  children: [
                    OutlinedButton.icon(
                      onPressed: () => _save(payload),
                      icon: const Icon(Icons.download),
                      label: const Text('保存到相册'),
                    ),
                    const SizedBox(width: 12),
                    FilledButton.icon(
                      onPressed: _reload,
                      icon: const Icon(Icons.refresh),
                      label: const Text('刷新'),
                    ),
                  ],
                ),
              ],
            ),
          ),
          const SizedBox(height: 24),
          ListTile(
            leading: const Icon(Icons.receipt_long),
            title: const Text('收款记录'),
            subtitle: const Text('查看最近的收款明细'),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => BillsPage(ports: widget.ports),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
