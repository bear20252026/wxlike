/// 资料页与群详情的一次性状态机。
///
/// 备注/星标/标签是本人私有偏好，写回用 `expected_version`；409 表示别处
/// 已经改过，界面必须刷新后再改，不能覆盖别人的修改。
library;

import 'package:flutter/foundation.dart';

import '../../shell/contracts.dart';
import 'models.dart';
import 'relations_ports.dart';

class ProfileController extends ChangeNotifier {
  ProfileController({required this.port, required this.userId});

  final RelationsPort port;
  final String userId;

  ContactProfile? profile;
  bool loading = false;
  bool saving = false;
  G12Failure? error;
  bool _disposed = false;

  Future<void> load() async {
    if (loading) return;
    loading = true;
    error = null;
    _notify();
    try {
      profile = await port.profile(userId);
    } catch (failure) {
      error = asG12Failure(failure, operation: '加载资料');
    } finally {
      loading = false;
      _notify();
    }
  }

  Future<bool> savePrefs({String? remark, bool? starred, List<String>? tags}) async {
    final current = profile;
    if (current == null || saving || current.isSelf) return false;
    saving = true;
    error = null;
    _notify();
    try {
      profile = await port.updatePrefs(
        userId,
        expectedVersion: current.version,
        remark: remark,
        starred: starred,
        tags: tags,
      );
      return true;
    } catch (failure) {
      final mapped = asG12Failure(failure, operation: '保存资料偏好');
      error = mapped;
      if (mapped.kind == G12FailureKind.conflict) {
        // 冲突时主动拉最新版本，用户下一次修改才有正确的 expected_version；
        // load() 会清空 error，所以刷新后要把冲突原因放回去。
        await load();
        error = mapped;
      }
      return false;
    } finally {
      saving = false;
      _notify();
    }
  }

  Future<bool> toggleStar() async {
    final current = profile;
    if (current == null) return false;
    return savePrefs(starred: !current.starred);
  }

  Future<bool> block() => _mutate('block');
  Future<bool> unblock() => _mutate('unblock');

  Future<bool> _mutate(String action) async {
    if (profile == null || saving) return false;
    saving = true;
    error = null;
    _notify();
    try {
      final key = newIdempotencyKey('friend-$action');
      profile = action == 'block'
          ? await port.block(userId, idempotencyKey: key)
          : await port.unblock(userId, idempotencyKey: key);
      return true;
    } catch (failure) {
      error = asG12Failure(failure, operation: action == 'block' ? '拉黑' : '解除拉黑');
      return false;
    } finally {
      saving = false;
      _notify();
    }
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

class GroupController extends ChangeNotifier {
  GroupController({required this.port, required this.groupId});

  final RelationsPort port;
  final String groupId;

  GroupDetail? detail;
  bool loading = false;
  bool mutating = false;
  G12Failure? error;
  bool _disposed = false;

  GroupSummary? get summary => detail?.summary;

  Future<void> load() async {
    if (loading) return;
    loading = true;
    error = null;
    _notify();
    try {
      detail = await port.group(groupId);
    } catch (failure) {
      error = asG12Failure(failure, operation: '加载群资料');
    } finally {
      loading = false;
      _notify();
    }
  }

  Future<bool> addMembers(List<String> members) => _mutate(
        (version, key) => port.addMembers(groupId, members, expectedVersion: version, idempotencyKey: key),
        '邀请成员',
        'group-add-member',
      );

  Future<bool> changeRole(String userId, GroupRole role) => _mutate(
        (version, key) => port.changeRole(groupId, userId, role, expectedVersion: version, idempotencyKey: key),
        '修改群角色',
        'group-role',
      );

  Future<bool> kick(String userId) => _mutate(
        (version, key) async {
          await port.kick(groupId, userId, expectedVersion: version, idempotencyKey: key);
          return port.group(groupId);
        },
        '移出成员',
        'group-kick',
      );

  Future<bool> leave() => _mutate(
        (version, key) async {
          await port.leave(groupId, idempotencyKey: key);
          return port.group(groupId);
        },
        '退出群聊',
        'group-leave',
        refreshAfter: false,
      );

  Future<bool> _mutate(
    Future<GroupDetail> Function(int version, String key) run,
    String operation,
    String keyPrefix, {
    bool refreshAfter = true,
  }) async {
    final current = detail;
    if (current == null || mutating) return false;
    mutating = true;
    error = null;
    _notify();
    try {
      detail = await run(current.summary.version, newIdempotencyKey(keyPrefix));
      if (refreshAfter) detail = await port.group(groupId);
      return true;
    } catch (failure) {
      error = asG12Failure(failure, operation: operation);
      if (error!.kind == G12FailureKind.conflict) await load();
      return false;
    } finally {
      mutating = false;
      _notify();
    }
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
