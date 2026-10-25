/// 会话草稿与待发附件。
///
/// 两件事必须跨进程活下来：
/// 1. 用户打到一半的文字（系统返回、切页、进程被杀）；
/// 2. 已经采集但还没送达的附件（离线时可恢复，而不是重新拍一次）。
///
/// 落盘只写应用私有目录，不写日志；token 与消息正文不经过这里。真正的
/// 消息事实仍在 Rust 的 outbox 里，这里只恢复"界面输入"。
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

import '../../../platform/platform_ports.dart';
import 'composer_ports.dart';

class AttachmentDraft {
  const AttachmentDraft({
    required this.ref,
    required this.kind,
    this.name = '',
    this.mimeType = '',
    this.bytes = 0,
    this.durationMs = 0,
    this.uploadedMediaId = '',
  });

  final String ref;
  final MediaKind kind;
  final String name;
  final String mimeType;
  final int bytes;
  final int durationMs;

  /// 空表示"还没上传成功"；非空表示失败发生在上传之后，重试只重发。
  final String uploadedMediaId;

  factory AttachmentDraft.fromMedia(CapturedMedia media) => AttachmentDraft(
    ref: media.ref,
    kind: media.kind,
    name: media.name,
    mimeType: media.mimeType,
    bytes: media.bytes,
    durationMs: media.durationMs,
  );

  AttachmentDraft withUploaded(String mediaId) => AttachmentDraft(
    ref: ref,
    kind: kind,
    name: name,
    mimeType: mimeType,
    bytes: bytes,
    durationMs: durationMs,
    uploadedMediaId: mediaId,
  );

  CapturedMedia toMedia() => CapturedMedia(
    ref: ref,
    kind: kind,
    name: name,
    mimeType: mimeType,
    bytes: bytes,
    durationMs: durationMs,
  );

  UploadedMedia? toUploaded() => uploadedMediaId.isEmpty
      ? null
      : UploadedMedia(
          mediaId: uploadedMediaId,
          kind: kind,
          durationMs: durationMs,
        );

  Map<String, Object?> toJson() => {
    'ref': ref,
    'kind': kind.name,
    'name': name,
    'mime_type': mimeType,
    'bytes': bytes,
    'duration_ms': durationMs,
    'uploaded_media_id': uploadedMediaId,
  };

  factory AttachmentDraft.fromJson(Map<String, Object?> json) =>
      AttachmentDraft(
        ref: json['ref'] as String? ?? '',
        kind: _kindFromWire(json['kind'] as String?),
        name: json['name'] as String? ?? '',
        mimeType: json['mime_type'] as String? ?? '',
        bytes: (json['bytes'] as num?)?.toInt() ?? 0,
        durationMs: (json['duration_ms'] as num?)?.toInt() ?? 0,
        uploadedMediaId: json['uploaded_media_id'] as String? ?? '',
      );

  static MediaKind _kindFromWire(String? value) => switch (value) {
    'image' => MediaKind.image,
    'video' => MediaKind.video,
    'audio' => MediaKind.audio,
    _ => MediaKind.file,
  };
}

abstract class DraftPersistence {
  Future<Map<String, Object?>> load();

  Future<void> save(Map<String, Object?> data);
}

/// 应用私有目录里的一个 JSON 文件。损坏时当作空草稿，不让应用起不来。
class FileDraftPersistence implements DraftPersistence {
  FileDraftPersistence({this.fileName = 'g12_drafts.json'});

  final String fileName;
  String? _resolvedPath;

  Future<String> _path() async {
    final cached = _resolvedPath;
    if (cached != null) return cached;
    final dir = await getApplicationSupportDirectory();
    return _resolvedPath = '${dir.path}${Platform.pathSeparator}$fileName';
  }

  @override
  Future<Map<String, Object?>> load() async {
    try {
      final file = File(await _path());
      if (!await file.exists()) return const {};
      final text = await file.readAsString();
      if (text.trim().isEmpty) return const {};
      final decoded = jsonDecode(text);
      return decoded is Map<String, Object?> ? decoded : const {};
    } catch (_) {
      return const {};
    }
  }

  @override
  Future<void> save(Map<String, Object?> data) async {
    final file = File(await _path());
    await file.parent.create(recursive: true);
    final tmp = File('${file.path}.tmp');
    await tmp.writeAsString(jsonEncode(data), flush: true);
    await tmp.rename(file.path);
  }
}

class MemoryDraftPersistence implements DraftPersistence {
  Map<String, Object?> data = const {};
  int loads = 0;
  int saves = 0;

  @override
  Future<Map<String, Object?>> load() async {
    loads++;
    return data;
  }

  @override
  Future<void> save(Map<String, Object?> next) async {
    saves++;
    data = next;
  }
}

class DraftStore extends ChangeNotifier {
  DraftStore({
    DraftPersistence? persistence,
    this.flushDelay = const Duration(milliseconds: 300),
  }) : _persistence = persistence ?? FileDraftPersistence();

  final DraftPersistence _persistence;
  final Duration flushDelay;

  final Map<String, String> _drafts = {};
  final Map<String, AttachmentDraft> _attachments = {};
  Timer? _timer;
  bool _loaded = false;
  Future<void>? _loading;
  Future<void>? _saving;

  bool get loaded => _loaded;

  /// 幂等；界面可以在任意时刻调用。
  Future<void> load() {
    return _loading ??= _load();
  }

  Future<void> _load() async {
    try {
      final data = await _persistence.load();
      final drafts = data['drafts'];
      if (drafts is Map) {
        for (final entry in drafts.entries) {
          final key = entry.key;
          final value = entry.value;
          if (key is String && value is String && value.isNotEmpty) {
            _drafts.putIfAbsent(key, () => value);
          }
        }
      }
      final attachments = data['attachments'];
      if (attachments is Map) {
        for (final entry in attachments.entries) {
          final key = entry.key;
          final value = entry.value;
          if (key is String && value is Map) {
            _attachments.putIfAbsent(
              key,
              () => AttachmentDraft.fromJson(
                value.map((key, value) => MapEntry('$key', value)),
              ),
            );
          }
        }
      }
    } finally {
      _loaded = true;
      notifyListeners();
    }
  }

  String read(String conv) => _drafts[conv] ?? '';

  bool hasDraft(String conv) => read(conv).trim().isNotEmpty;

  void write(String conv, String text) {
    if (text.isEmpty) {
      if (_drafts.remove(conv) != null) {
        _schedule();
        notifyListeners();
      }
      return;
    }
    if (_drafts[conv] == text) return;
    _drafts[conv] = text;
    _schedule();
    notifyListeners();
  }

  void clear(String conv) {
    final removed = _drafts.remove(conv) != null;
    final removedAttachment = _attachments.remove(conv) != null;
    if (removed || removedAttachment) {
      _schedule();
      notifyListeners();
    }
  }

  AttachmentDraft? attachment(String conv) => _attachments[conv];

  void putAttachment(String conv, AttachmentDraft draft) {
    _attachments[conv] = draft;
    _schedule();
    notifyListeners();
  }

  void clearAttachment(String conv) {
    if (_attachments.remove(conv) != null) {
      _schedule();
      notifyListeners();
    }
  }

  void _schedule() {
    _timer?.cancel();
    _timer = Timer(flushDelay, () => unawaited(flush()));
  }

  /// 立刻落盘（切后台、退出页面时调用）。
  Future<void> flush() async {
    _timer?.cancel();
    _timer = null;
    if (!_loaded) {
      await (_loading ??= _load());
    }
    final pending = _saving;
    if (pending != null) {
      await pending;
      return;
    }
    final snapshot = <String, Object?>{
      'version': 1,
      'drafts': Map<String, String>.from(_drafts),
      'attachments': {
        for (final entry in _attachments.entries)
          entry.key: entry.value.toJson(),
      },
    };
    _saving = _persistence
        .save(snapshot)
        .catchError((Object _) {
          // 落盘失败不回滚内存草稿：用户还能继续输入，下次再试。
        })
        .whenComplete(() => _saving = null);
    await _saving;
  }

  @override
  void dispose() {
    _timer?.cancel();
    unawaited(flush());
    super.dispose();
  }
}
