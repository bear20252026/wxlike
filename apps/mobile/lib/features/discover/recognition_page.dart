/// G13-04：识物与翻译。
///
/// 图片解码仍由平台桥提供（G12-05），这里只把引用交给 `discovery.recognize`；
/// 文本翻译走 `discovery.translate`，结果语言由用户选、识别结果由服务端判。
/// 未接线时显示阻塞原因，不拿静态示例充当识别结果。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

/// 返回一张待识别图片的引用（对象 id 或本地临时路径）。
typedef RecognizeImageProvider = Future<String?> Function();

/// 目标语言。值直接作为 `targetLanguage` 传给服务端，界面不自建翻译表。
const List<(String, String)> _languages = [
  ('zh', '中文'),
  ('en', '英文'),
  ('ja', '日文'),
  ('ko', '韩文'),
];

class RecognitionPage extends StatefulWidget {
  const RecognitionPage({super.key, required this.ports, this.capture});

  final G13Ports ports;
  final RecognizeImageProvider? capture;

  @override
  State<RecognitionPage> createState() => _RecognitionPageState();
}

class _RecognitionPageState extends State<RecognitionPage> {
  final TextEditingController _source = TextEditingController();
  String _target = _languages.first.$1;
  bool _recognizing = false;
  bool _translating = false;
  String _imageRef = '';
  RecognitionResult? _result;
  String _resultKind = 'recognize';
  Object? _error;
  String _errorFrom = '';

  @override
  void dispose() {
    _source.dispose();
    super.dispose();
  }

  Future<void> _pickAndRecognize() async {
    final capture = widget.capture;
    if (capture == null) {
      setState(() {
        _error = const G13Failure(
          G13FailureKind.unavailable,
          'CAMERA_NOT_WIRED',
          '相机与图片解码由平台桥提供，当前未接入',
        );
        _errorFrom = 'recognize';
        _result = null;
      });
      return;
    }
    if (_recognizing) return;
    setState(() {
      _recognizing = true;
      _error = null;
      _errorFrom = '';
    });
    try {
      final ref = await capture();
      if (ref == null || ref.isEmpty) return;
      _imageRef = ref;
      final result = await widget.ports.discovery.recognize(
        ref,
        targetLanguage: _target,
      );
      if (!mounted) return;
      setState(() {
        _result = result;
        _resultKind = 'recognize';
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _error = error;
        _errorFrom = 'recognize';
      });
    } finally {
      if (mounted) setState(() => _recognizing = false);
    }
  }

  /// 重试只重发同一个图片引用，不重新弹相机；换图由用户重新选择。
  Future<void> _retryRecognize() async {
    if (_imageRef.isEmpty) return;
    if (_recognizing) return;
    setState(() {
      _recognizing = true;
      _error = null;
      _errorFrom = '';
    });
    try {
      final result = await widget.ports.discovery.recognize(
        _imageRef,
        targetLanguage: _target,
      );
      if (!mounted) return;
      setState(() {
        _result = result;
        _resultKind = 'recognize';
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _error = error;
        _errorFrom = 'recognize';
      });
    } finally {
      if (mounted) setState(() => _recognizing = false);
    }
  }

  Future<void> _retryTranslate() async {
    final text = _source.text.trim();
    if (text.isEmpty) {
      setState(() {
        _error = const G13Failure(
          G13FailureKind.invalidArgument,
          'EMPTY_TEXT',
          '请输入要翻译的内容',
        );
        _errorFrom = 'translate';
      });
      return;
    }
    if (_translating) return;
    setState(() {
      _translating = true;
      _error = null;
      _errorFrom = '';
    });
    try {
      final result = await widget.ports.discovery.translate(
        text,
        targetLanguage: _target,
      );
      if (!mounted) return;
      setState(() {
        _result = result;
        _resultKind = 'translate';
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _error = error;
        _errorFrom = 'translate';
      });
    } finally {
      if (mounted) setState(() => _translating = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final busy = _recognizing || _translating;
    final result = _result;
    return G13Scaffold(
      title: '识物与翻译',
      onBack: () => Navigator.of(context).maybePop(),
      body: ListView(
        padding: const EdgeInsets.all(20),
        children: [
          Row(
            children: [
              const Text('目标语言'),
              const SizedBox(width: 12),
              DropdownButton<String>(
                value: _target,
                onChanged: busy
                    ? null
                    : (value) {
                        if (value == null) return;
                        setState(() => _target = value);
                      },
                items: [
                  for (final (code, label) in _languages)
                    DropdownMenuItem<String>(value: code, child: Text(label)),
                ],
              ),
            ],
          ),
          const SizedBox(height: 12),
          Container(
            height: 180,
            decoration: BoxDecoration(
              color: Colors.black87,
              borderRadius: BorderRadius.circular(12),
            ),
            alignment: Alignment.center,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Icon(Icons.image_search, color: Colors.white54, size: 44),
                const SizedBox(height: 8),
                Text(
                  widget.capture == null ? '相机未接入（需 G12-05 平台桥）' : '拍照或选择图片',
                  style: const TextStyle(color: Colors.white54),
                ),
              ],
            ),
          ),
          const SizedBox(height: 12),
          FilledButton.icon(
            onPressed: busy ? null : _pickAndRecognize,
            icon: _recognizing
                ? const SizedBox(
                    width: 18,
                    height: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.camera_alt_outlined),
            label: Text(widget.capture == null ? '选择图片（未接入）' : '识别图片'),
          ),
          const Divider(height: 40),
          TextField(
            controller: _source,
            minLines: 3,
            maxLines: 6,
            maxLength: 500,
            enabled: !busy,
            decoration: const InputDecoration(
              labelText: '或输入文本翻译',
              border: OutlineInputBorder(),
            ),
            textInputAction: TextInputAction.newline,
          ),
          const SizedBox(height: 4),
          FilledButton.tonalIcon(
            onPressed: busy ? null : _retryTranslate,
            icon: _translating
                ? const SizedBox(
                    width: 18,
                    height: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.translate),
            label: const Text('翻译'),
          ),
          if (_error != null) ...[
            const SizedBox(height: 16),
            FailureView(
              error: _error!,
              onRetry: busy
                  ? null
                  : _errorFrom == 'recognize'
                  ? (_imageRef.isEmpty ? null : _retryRecognize)
                  : _retryTranslate,
            ),
          ],
          if (result != null) ...[
            const SizedBox(height: 20),
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        Expanded(
                          child: Text(
                            result.label.isEmpty
                                ? (_resultKind == 'translate' ? '翻译结果' : '识别结果')
                                : result.label,
                            style: Theme.of(context).textTheme.titleMedium,
                          ),
                        ),
                        if (result.confidence > 0)
                          Pill(
                            text:
                                '置信度 ${(result.confidence * 100).toStringAsFixed(0)}%',
                          ),
                      ],
                    ),
                    if (result.sourceText.isNotEmpty) ...[
                      const SizedBox(height: 8),
                      Text(result.sourceText),
                    ],
                    if (result.translation.isNotEmpty) ...[
                      const SizedBox(height: 12),
                      Text(
                        result.translation,
                        style: Theme.of(context).textTheme.bodyLarge,
                      ),
                    ],
                    const SizedBox(height: 8),
                    Text(
                      '目标语言：${result.targetLanguage}',
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ],
                ),
              ),
            ),
          ],
        ],
      ),
    );
  }
}
