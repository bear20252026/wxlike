/// G13-04：扫一扫。
///
/// 相机帧与相册解码由平台桥提供（G12-05），这里只做安全深链解析与跳转。
/// 解析结果必须经过 `safe` 校验：不安全的目标只展示原文，不自动打开；即使
/// 是收款码也只进入收付款页面，不自动扣款。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';
import '../commerce/pay_page.dart';
import '../mini/mini_programs_page.dart';

/// 返回一个相机帧/相册图片的引用（对象 id 或本地临时路径）。
typedef ImageRefProvider = Future<String?> Function();

class ScanPage extends StatefulWidget {
  const ScanPage({super.key, required this.ports, this.capture});

  final G13Ports ports;
  final ImageRefProvider? capture;

  @override
  State<ScanPage> createState() => _ScanPageState();
}

class _ScanPageState extends State<ScanPage> {
  ScanResult? _result;
  Object? _error;
  bool _busy = false;

  Future<void> _scan() async {
    final capture = widget.capture;
    if (capture == null) {
      setState(() {
        _error = const G13Failure(
          G13FailureKind.unavailable,
          'CAMERA_NOT_WIRED',
          '相机与相册解码由平台桥提供，当前未接入',
        );
      });
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final ref = await capture();
      if (ref == null || ref.isEmpty) {
        if (mounted) setState(() => _busy = false);
        return;
      }
      final result = await widget.ports.discovery.decodeScan(ref);
      if (!mounted) return;
      setState(() => _result = result);
    } catch (error) {
      if (!mounted) return;
      setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  void _openResult(ScanResult result) {
    if (!result.safe) {
      ScaffoldMessenger.of(context)
          .showSnackBar(SnackBar(content: Text('目标未通过安全校验：${result.reason}')));
      return;
    }
    switch (result.kind) {
      case 'payment':
        Navigator.of(context).push(
          MaterialPageRoute<void>(builder: (_) => PayPage(ports: widget.ports)),
        );
      case 'mini':
        Navigator.of(context).push(
          MaterialPageRoute<void>(
            builder: (_) => MiniProgramsPage(ports: widget.ports),
          ),
        );
      default:
        showModalBottomSheet<void>(
          context: context,
          showDragHandle: true,
          builder: (context) => SafeArea(
            child: Padding(
              padding: const EdgeInsets.all(20),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('扫描结果', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 8),
                  SelectableText(result.raw),
                  const SizedBox(height: 12),
                  Text(
                    '类型：${result.kind} · 目标：${result.targetId}',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                ],
              ),
            ),
          ),
        );
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '扫一扫',
      onBack: () => Navigator.of(context).maybePop(),
      body: ListView(
        padding: const EdgeInsets.all(20),
        children: [
          Container(
            height: 220,
            decoration: BoxDecoration(
              color: Colors.black87,
              borderRadius: BorderRadius.circular(12),
            ),
            alignment: Alignment.center,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Icon(
                  Icons.qr_code_scanner,
                  color: Colors.white54,
                  size: 44,
                ),
                const SizedBox(height: 8),
                Text(
                  widget.capture == null ? '相机未接入（需 G12-05 平台桥）' : '对准二维码/条形码',
                  style: const TextStyle(color: Colors.white54),
                ),
              ],
            ),
          ),
          const SizedBox(height: 16),
          FilledButton.icon(
            onPressed: _busy ? null : _scan,
            icon: _busy
                ? const SizedBox(
                    width: 18,
                    height: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.camera_alt_outlined),
            label: Text(widget.capture == null ? '从相册选择（未接入）' : '开始扫描'),
          ),
          if (_error != null) ...[
            const SizedBox(height: 12),
            FailureView(error: _error!, onRetry: _scan),
          ],
          if (_result != null) ...[
            const SizedBox(height: 20),
            Card(
              child: ListTile(
                leading: Icon(
                  _result!.safe
                      ? Icons.verified_outlined
                      : Icons.gpp_bad_outlined,
                ),
                title: Text(
                  _result!.raw,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                ),
                subtitle: Text(
                  _result!.safe
                      ? '类型：${_result!.kind}'
                      : '未通过安全校验：${_result!.reason}',
                ),
                trailing: _result!.safe
                    ? const Icon(Icons.chevron_right)
                    : null,
                onTap: _result!.safe ? () => _openResult(_result!) : null,
              ),
            ),
          ],
        ],
      ),
    );
  }
}
