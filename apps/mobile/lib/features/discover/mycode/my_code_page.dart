/// G13-06：我的二维码（名片码）。
///
/// 四种样式的 payload 都由 `contact.qr.*` 生成；`scannable` 为 false 时不能
/// 宣称"可扫"。码图渲染与相册写入分别由平台桥和 `contact.qr.save` 完成，
/// 渲染未接入时展示阻塞占位而不是画一个假二维码。
library;

import 'package:flutter/material.dart';

import '../../content/g13/core.dart';
import '../../content/g13/models.dart';
import '../../content/g13/ports.dart';
import '../../content/g13/widgets.dart';

/// 渲染标准二维码；必须产出可被系统扫码器解码的图。
typedef QrRenderer = Widget Function(
  BuildContext context,
  String payload,
  QrStyle style,
);

/// 调起系统分享；返回是否真的分享成功。
typedef QrSharer = Future<bool> Function(String payload, QrStyle style);

class MyCodePage extends StatefulWidget {
  const MyCodePage({
    super.key,
    required this.ports,
    this.qrBuilder,
    this.share,
  });

  final G13Ports ports;
  final QrRenderer? qrBuilder;
  final QrSharer? share;

  @override
  State<MyCodePage> createState() => _MyCodePageState();
}

class _MyCodePageState extends State<MyCodePage> {
  QrStyle _style = QrStyle.spring;
  late Future<MyQrCode> _future = _load();
  MyQrCode? _code;
  Object? _error;
  bool _saving = false;
  bool _rotating = false;

  Future<MyQrCode> _load() async {
    final code = await widget.ports.qr.myCode(
      _style,
      idempotencyKey: 'qr-view-${_style.name}',
    );
    if (mounted) setState(() => _code = code);
    return code;
  }

  void _reload() {
    if (!mounted) return;
    setState(() {
      _error = null;
      _future = _load();
    });
  }

  void _selectStyle(QrStyle style) {
    if (_rotating || _saving || style == _style) return;
    setState(() {
      _style = style;
      _code = null;
      _error = null;
      _future = _load();
    });
  }

  Future<void> _save(MyQrCode code) async {
    if (_saving) return;
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      await widget.ports.qr.saveToAlbum(
        code.payload,
        idempotencyKey: 'qr-save-${code.version}-${code.style.name}',
      );
      if (!mounted) return;
      ScaffoldMessenger.of(context)
          .showSnackBar(const SnackBar(content: Text('已保存到相册')));
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  Future<void> _share(MyQrCode code) async {
    final sharer = widget.share;
    if (sharer == null) {
      ScaffoldMessenger.of(context)
          .showSnackBar(const SnackBar(content: Text('系统分享由平台桥提供，当前未接入')));
      return;
    }
    try {
      final ok = await sharer(code.payload, code.style);
      if (!mounted) return;
      if (!ok) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('分享未完成')));
      }
    } catch (error) {
      if (!mounted) return;
      final text = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    }
  }

  Future<void> _rotate() async {
    if (_rotating) return;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('更换二维码'),
        content: const Text('更换后旧码立即失效，已分享出去的旧码将无法添加你。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('更换'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    setState(() {
      _rotating = true;
      _error = null;
    });
    try {
      final rotated = await widget.ports.qr.rotate(
        idempotencyKey: newIdempotencyKey(),
      );
      if (!mounted) return;
      setState(() {
        _code = rotated;
        _style = rotated.style;
        _future = Future<MyQrCode>.value(rotated);
      });
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _rotating = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '我的二维码',
      onBack: () => Navigator.of(context).maybePop(),
      body: AsyncView<MyQrCode>(
        future: _future,
        onRetry: _reload,
        builder: (context, loaded) => _body(_code ?? loaded),
      ),
    );
  }

  Widget _body(MyQrCode code) {
    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        if (_error != null)
          InlineFailure(
            error: _error!,
            onRetry: _error is G13Failure && (_error! as G13Failure).retryable
                ? _reload
                : null,
          ),
        Center(
          child: Column(
            children: [
              Text(
                '扫一扫上面的二维码，添加我为朋友',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 12),
              _qrSurface(code),
              const SizedBox(height: 12),
              if (!code.scannable)
                const Pill(text: '服务端未确认该码可扫码', color: Colors.orange),
              Text(
                '样式：${code.style.label} · 版本 ${code.version}',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              Text(
                '有效期至 ${formatTime(code.expiresAt)}',
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
          ),
        ),
        const SizedBox(height: 16),
        Wrap(
          alignment: WrapAlignment.center,
          spacing: 8,
          children: [
            for (final style in QrStyle.values)
              ChoiceChip(
                label: Text(style.label),
                selected: _style == style,
                onSelected: _rotating || _saving
                    ? null
                    : (_) => _selectStyle(style),
              ),
          ],
        ),
        const SizedBox(height: 16),
        Row(
          children: [
            Expanded(
              child: FilledButton.tonalIcon(
                onPressed: _saving ? null : () => _save(code),
                icon: const Icon(Icons.download_outlined),
                label: Text(_saving ? '保存中…' : '保存到相册'),
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: OutlinedButton.icon(
                onPressed: () => _share(code),
                icon: const Icon(Icons.share_outlined),
                label: const Text('分享'),
              ),
            ),
          ],
        ),
        const SizedBox(height: 12),
        OutlinedButton.icon(
          onPressed: _rotating ? null : _rotate,
          icon: const Icon(Icons.autorenew),
          label: Text(_rotating ? '更换中…' : '更换二维码'),
        ),
        const SizedBox(height: 24),
        SelectableText(
          code.payload,
          style: Theme.of(context).textTheme.bodySmall,
        ),
      ],
    );
  }

  Widget _qrSurface(MyQrCode code) {
    final builder = widget.qrBuilder;
    if (builder != null) {
      return Container(
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: Colors.white,
          borderRadius: BorderRadius.circular(12),
        ),
        child: builder(context, code.payload, code.style),
      );
    }
    return Container(
      width: 220,
      height: 220,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: Theme.of(context).colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(12),
      ),
      child: const Padding(
        padding: EdgeInsets.all(16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.qr_code_2, size: 48),
            SizedBox(height: 8),
            Text('二维码渲染由平台桥提供，当前未接入（G12-05）', textAlign: TextAlign.center),
            SizedBox(height: 4),
            Text(
              'payload 已生成，接入后显示标准可扫码图',
              textAlign: TextAlign.center,
              style: TextStyle(fontSize: 12),
            ),
          ],
        ),
      ),
    );
  }
}
