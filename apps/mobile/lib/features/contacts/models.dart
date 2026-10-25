/// 联系人/群资料的视图模型。
///
/// 字段与 `relations-v1` 契约一一对应；权限相关字段（`isSelf`、`role`、
/// `canManage`）由上游判定，页面不再自己猜。
library;

enum FriendRequestStatus { pending, accepted, rejected, cancelled }

class Contact {
  const Contact({
    required this.userId,
    required this.displayName,
    this.remark = '',
    this.starred = false,
    this.tags = const [],
    this.blocked = false,
    this.isFriend = true,
    this.version = 0,
  });

  final String userId;
  final String displayName;

  /// 备注是单方面私有数据，只在本人的通讯录里生效。
  final String remark;
  final bool starred;
  final List<String> tags;
  final bool blocked;
  final bool isFriend;

  /// 私有偏好版本；写回时用于乐观并发（conflict 表示别处已改）。
  final int version;

  String get title => remark.isNotEmpty ? remark : displayName;

  String get indexLetter {
    final source = title.trim();
    if (source.isEmpty) return '#';
    final char = source.substring(0, 1).toUpperCase();
    return RegExp(r'[A-Z]').hasMatch(char) ? char : '#';
  }
}

class FriendRequest {
  const FriendRequest({
    required this.requestId,
    required this.fromUserId,
    required this.displayName,
    required this.status,
    required this.createdAtUtc,
    this.message = '',
    this.fromRemark = '',
  });

  final String requestId;
  final String fromUserId;
  final String displayName;
  final String message;
  final String fromRemark;
  final FriendRequestStatus status;
  final DateTime createdAtUtc;

  bool get pending => status == FriendRequestStatus.pending;

  String get title => fromRemark.isNotEmpty ? fromRemark : displayName;
}

class ContactProfile {
  const ContactProfile({
    required this.userId,
    required this.displayName,
    required this.isSelf,
    this.remark = '',
    this.tags = const [],
    this.starred = false,
    this.blocked = false,
    this.isFriend = false,
    this.bio = '',
    this.commonGroups = const [],
    this.version = 0,
  });

  final String userId;
  final String displayName;
  final bool isSelf;
  final String remark;
  final List<String> tags;
  final bool starred;
  final bool blocked;
  final bool isFriend;
  final String bio;
  final List<String> commonGroups;
  final int version;

  String get title => remark.isNotEmpty ? remark : displayName;
}

enum GroupRole { owner, admin, member }

GroupRole roleFromWire(String value) => switch (value) {
      'owner' => GroupRole.owner,
      'admin' => GroupRole.admin,
      _ => GroupRole.member,
    };

String roleToWire(GroupRole role) => switch (role) {
      GroupRole.owner => 'owner',
      GroupRole.admin => 'admin',
      GroupRole.member => 'member',
    };

class GroupSummary {
  const GroupSummary({
    required this.groupId,
    required this.name,
    required this.memberCount,
    required this.role,
    this.visibility = 'public',
    this.version = 0,
  });

  final String groupId;
  final String name;
  final int memberCount;
  final GroupRole role;
  final String visibility;
  final int version;

  bool get canManage => role == GroupRole.owner || role == GroupRole.admin;
}

class GroupMember {
  const GroupMember({
    required this.userId,
    required this.displayName,
    required this.role,
    this.remark = '',
  });

  final String userId;
  final String displayName;
  final String remark;
  final GroupRole role;

  String get title => remark.isNotEmpty ? remark : displayName;
}

class GroupDetail {
  const GroupDetail({
    required this.summary,
    required this.members,
    this.myRole = GroupRole.member,
  });

  final GroupSummary summary;
  final List<GroupMember> members;
  final GroupRole myRole;

  bool get canManage => myRole == GroupRole.owner || myRole == GroupRole.admin;
}
