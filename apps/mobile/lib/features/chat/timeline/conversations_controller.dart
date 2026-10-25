import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../../shell/contracts.dart';
import '../../../src/service.dart';
import 'messaging_ports.dart';

class ConversationsController extends ChangeNotifier {
  ConversationsController({
    required this.port,
    this.watchInterval = const Duration(seconds: 3),
  });

  final MessagingPort port;
  final Duration watchInterval;

  List<ConversationRow> rows = const [];
  LiveInfo? live;
  bool fromCache = false;
  bool busy = false;
  G12Failure? error;
  String query = '';

  StreamSubscription<ChatEvent>? _sub;
  bool _foreground = true;
  bool _disposed = false;

  List<ConversationRow> get visibleRows {
    final text = query.trim().toLowerCase();
    if (text.isEmpty) return rows;
    return rows
        .where((row) =>
            row.title.toLowerCase().contains(text) || row.conv.toLowerCase().contains(text))
        .toList(growable: false);
  }

  Future<void> load() async {
    if (busy) return;
    busy = true;
    error = null;
    _notify();
    try {
      try {
        live = await port.connect();
      } catch (failure) {
        // 离线也要显示本地缓存列表；把原因留在 banner 上。
        error = asG12Failure(failure, operation: '连接服务端');
      }
      final list = await port.conversations();
      live = await port.liveInfo();
      if (_disposed) return;
      rows = List<ConversationRow>.unmodifiable(list.conversations);
      fromCache = list.fromCache;
      _startWatch();
    } catch (failure) {
      if (!_disposed) error = asG12Failure(failure, operation: '加载会话列表');
    } finally {
      busy = false;
      _notify();
    }
  }

  void setQuery(String value) {
    query = value;
    _notify();
  }

  void setForeground(bool foreground) {
    if (_foreground == foreground) return;
    _foreground = foreground;
    if (foreground) {
      _startWatch();
      unawaited(load());
    } else {
      _sub?.cancel();
      _sub = null;
    }
  }

  void _startWatch() {
    if (_disposed || !_foreground) return;
    _sub?.cancel();
    _sub = port.watch('', interval: watchInterval).listen((event) {
      if (_disposed) return;
      switch (event) {
        case ConversationsUpdated(:final list, :final live):
          rows = List<ConversationRow>.unmodifiable(list.conversations);
          fromCache = list.fromCache;
          this.live = live;
          if (!list.fromCache) error = null;
          _notify();
        case ChatWatchFailed(:final failure):
          error = failure;
          _notify();
        case MessagesUpdated():
        case UnreadUpdated():
        case OutboxUpdated():
          break;
      }
    });
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
