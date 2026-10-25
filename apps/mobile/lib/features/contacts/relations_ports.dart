/// 关系域端口：好友、审批、私有偏好、群成员与角色。
///
/// 命令名对齐 `contracts/domains/relations/contract.json`。当前 FFI 绑定只
/// 导出旧版 26 条命令，关系域尚未进入生成绑定；[UnavailableRelationsPort]
/// 让每个入口如实报"未接线"，而不是返回一份本地假通讯录。
library;

import '../../shell/contracts.dart';
import 'models.dart';

abstract class RelationsPort {
  Future<PageResult<Contact>> friends({String cursor = '', int limit = 50});

  Future<PageResult<Contact>> search(String query, {String cursor = '', int limit = 50});

  Future<PageResult<FriendRequest>> requests({String cursor = '', int limit = 50});

  Future<FriendRequest> acceptRequest(String requestId, {required String idempotencyKey});

  Future<FriendRequest> rejectRequest(String requestId, {required String idempotencyKey});

  Future<void> cancelRequest(String requestId, {required String idempotencyKey});

  Future<ContactProfile> profile(String userId);

  Future<ContactProfile> updatePrefs(
    String userId, {
    required int expectedVersion,
    String? remark,
    bool? starred,
    List<String>? tags,
  });

  Future<ContactProfile> block(String userId, {required String idempotencyKey});

  Future<ContactProfile> unblock(String userId, {required String idempotencyKey});

  Future<void> removeFriend(String userId, {required String idempotencyKey});

  Future<PageResult<GroupSummary>> groups({String cursor = '', int limit = 50});

  Future<GroupDetail> group(String groupId);

  Future<GroupDetail> addMembers(
    String groupId,
    List<String> members, {
    required int expectedVersion,
    required String idempotencyKey,
  });

  Future<GroupDetail> changeRole(
    String groupId,
    String userId,
    GroupRole role, {
    required int expectedVersion,
    required String idempotencyKey,
  });

  Future<void> kick(
    String groupId,
    String userId, {
    required int expectedVersion,
    required String idempotencyKey,
  });

  Future<void> leave(String groupId, {required String idempotencyKey});
}

class UnavailableRelationsPort implements RelationsPort {
  const UnavailableRelationsPort();

  Never _notWired(String command) =>
      throw G12Failure.upstreamNotWired(command, '$command 尚未接入 G02 生成绑定');

  @override
  Future<PageResult<Contact>> friends({String cursor = '', int limit = 50}) =>
      _notWired('friend.list');

  @override
  Future<PageResult<Contact>> search(String query, {String cursor = '', int limit = 50}) =>
      _notWired('friend.search');

  @override
  Future<PageResult<FriendRequest>> requests({String cursor = '', int limit = 50}) =>
      _notWired('friend.requests');

  @override
  Future<FriendRequest> acceptRequest(String requestId, {required String idempotencyKey}) =>
      _notWired('friend.accept');

  @override
  Future<FriendRequest> rejectRequest(String requestId, {required String idempotencyKey}) =>
      _notWired('friend.reject');

  @override
  Future<void> cancelRequest(String requestId, {required String idempotencyKey}) =>
      _notWired('friend.cancel');

  @override
  Future<ContactProfile> profile(String userId) => _notWired('identity.profile.get');

  @override
  Future<ContactProfile> updatePrefs(
    String userId, {
    required int expectedVersion,
    String? remark,
    bool? starred,
    List<String>? tags,
  }) =>
      _notWired('friend.prefs.patch');

  @override
  Future<ContactProfile> block(String userId, {required String idempotencyKey}) =>
      _notWired('friend.block');

  @override
  Future<ContactProfile> unblock(String userId, {required String idempotencyKey}) =>
      _notWired('friend.unblock');

  @override
  Future<void> removeFriend(String userId, {required String idempotencyKey}) =>
      _notWired('friend.remove');

  @override
  Future<PageResult<GroupSummary>> groups({String cursor = '', int limit = 50}) =>
      _notWired('group.list');

  @override
  Future<GroupDetail> group(String groupId) => _notWired('group.info');

  @override
  Future<GroupDetail> addMembers(
    String groupId,
    List<String> members, {
    required int expectedVersion,
    required String idempotencyKey,
  }) =>
      _notWired('group.members.add');

  @override
  Future<GroupDetail> changeRole(
    String groupId,
    String userId,
    GroupRole role, {
    required int expectedVersion,
    required String idempotencyKey,
  }) =>
      _notWired('group.role.update');

  @override
  Future<void> kick(
    String groupId,
    String userId, {
    required int expectedVersion,
    required String idempotencyKey,
  }) =>
      _notWired('group.member.kick');

  @override
  Future<void> leave(String groupId, {required String idempotencyKey}) =>
      _notWired('group.leave');
}
