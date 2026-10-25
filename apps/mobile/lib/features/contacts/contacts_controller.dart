/// 通讯录投影：好友分页、搜索、新朋友审批、群列表。
///
/// 所有写操作完成后重新查询上游，不把"点了按钮"当成"关系已变"。
/// 同一次审批重试复用同一个幂等键；网络失败后不会换键再提交一次。
library;

import 'package:flutter/foundation.dart';

import '../../shell/contracts.dart';
import 'models.dart';
import 'relations_ports.dart';

enum ContactsSection { friends, requests, groups }

class ContactsController extends ChangeNotifier {
  ContactsController({required this.port});

  final RelationsPort port;

  ContactsSection section = ContactsSection.friends;
  List<Contact> contacts = const [];
  List<FriendRequest> requests = const [];
  List<GroupSummary> groups = const [];

  bool loading = false;
  bool loadingMore = false;
  bool loadingRequests = false;
  bool loadingGroups = false;
  G12Failure? error;
  G12Failure? requestError;
  G12Failure? groupError;

  String query = '';
  String _cursor = '';
  bool hasMore = false;
  int _searchGeneration = 0;

  final Set<String> deciding = {};
  final Map<String, String> _decisionKeys = {};

  bool _disposed = false;

  void select(ContactsSection next) {
    section = next;
    error = null;
    requestError = null;
    groupError = null;
    _notify();
    switch (next) {
      case ContactsSection.friends:
        if (contacts.isEmpty) load();
      case ContactsSection.requests:
        loadRequests();
      case ContactsSection.groups:
        loadGroups();
    }
  }

  Future<void> load({bool refresh = false}) async {
    if (loading) return;
    loading = true;
    error = null;
    _notify();
    try {
      final page = await port.friends(limit: 50);
      if (_disposed) return;
      contacts = List<Contact>.unmodifiable(_dedupe(page.items));
      _cursor = page.nextCursor;
      hasMore = page.hasMore;
    } catch (failure) {
      _error(failure, '加载联系人');
    } finally {
      loading = false;
      _notify();
    }
  }

  Future<void> loadMore() async {
    if (loading || loadingMore || !hasMore) return;
    loadingMore = true;
    _notify();
    try {
      final page = await port.friends(cursor: _cursor, limit: 50);
      if (_disposed) return;
      contacts = List<Contact>.unmodifiable(_dedupe([...contacts, ...page.items]));
      _cursor = page.nextCursor;
      hasMore = page.hasMore;
    } catch (failure) {
      _error(failure, '加载更多联系人');
    } finally {
      loadingMore = false;
      _notify();
    }
  }

  Future<void> search(String value) async {
    query = value;
    final generation = ++_searchGeneration;
    error = null;
    _notify();
    final text = value.trim();
    if (text.isEmpty) {
      await load();
      return;
    }
    loading = true;
    _notify();
    try {
      final page = await port.search(text, limit: 50);
      if (_disposed || generation != _searchGeneration) return;
      contacts = List<Contact>.unmodifiable(_dedupe(page.items));
      _cursor = page.nextCursor;
      hasMore = page.hasMore;
    } catch (failure) {
      if (generation == _searchGeneration) _error(failure, '搜索联系人');
    } finally {
      if (generation == _searchGeneration) {
        loading = false;
        _notify();
      }
    }
  }

  Future<void> loadRequests() async {
    if (loadingRequests) return;
    loadingRequests = true;
    requestError = null;
    _notify();
    try {
      final page = await port.requests(limit: 100);
      if (_disposed) return;
      requests = List<FriendRequest>.unmodifiable(page.items);
    } catch (failure) {
      requestError = asG12Failure(failure, operation: '加载新朋友');
    } finally {
      loadingRequests = false;
      _notify();
    }
  }

  /// 返回 null 表示动作没有执行（重复点击/未知请求）；异常通过 [requestError] 暴露。
  Future<FriendRequest?> decide(FriendRequest request, {required bool accept}) async {
    if (!request.pending || deciding.contains(request.requestId)) return null;
    deciding.add(request.requestId);
    requestError = null;
    _notify();
    final key = _decisionKeys.putIfAbsent(
      request.requestId,
      () => newIdempotencyKey(accept ? 'friend-accept' : 'friend-reject'),
    );
    try {
      final updated = accept
          ? await port.acceptRequest(request.requestId, idempotencyKey: key)
          : await port.rejectRequest(request.requestId, idempotencyKey: key);
      _decisionKeys.remove(request.requestId);
      // 重新查询，避免本地拼一份"已通过"的假状态。
      await Future.wait([loadRequests(), load(refresh: true)]);
      return updated;
    } catch (failure) {
      requestError = asG12Failure(failure, operation: accept ? '通过好友申请' : '拒绝好友申请');
      return null;
    } finally {
      deciding.remove(request.requestId);
      _notify();
    }
  }

  Future<void> loadGroups() async {
    if (loadingGroups) return;
    loadingGroups = true;
    groupError = null;
    _notify();
    try {
      final page = await port.groups(limit: 100);
      if (_disposed) return;
      groups = List<GroupSummary>.unmodifiable(page.items);
    } catch (failure) {
      groupError = asG12Failure(failure, operation: '加载群聊');
    } finally {
      loadingGroups = false;
      _notify();
    }
  }

  void _error(Object failure, String operation) {
    error = asG12Failure(failure, operation: operation);
  }

  List<Contact> _dedupe(List<Contact> items) {
    final seen = <String>{};
    return [
      for (final item in items)
        if (seen.add(item.userId)) item,
    ];
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
