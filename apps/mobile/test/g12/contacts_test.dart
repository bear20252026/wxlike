import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/contacts/contacts_controller.dart';
import 'package:wxlike_mobile/features/contacts/models.dart';
import 'package:wxlike_mobile/features/contacts/profile_controller.dart';
import 'package:wxlike_mobile/features/contacts/relations_ports.dart';
import 'package:wxlike_mobile/shell/contracts.dart';

Contact contact(String id, {bool starred = false, String remark = ''}) =>
    Contact(
      userId: id,
      displayName: id.toUpperCase(),
      starred: starred,
      remark: remark,
    );

FriendRequest request(
  String id, {
  FriendRequestStatus status = FriendRequestStatus.pending,
}) => FriendRequest(
  requestId: id,
  fromUserId: 'u-$id',
  displayName: '用户 $id',
  status: status,
  createdAtUtc: DateTime.utc(2026, 10, 24),
);

class FakeRelations implements RelationsPort {
  List<PageResult<Contact>> friendPages = [];
  int friendPage = 0;
  List<FriendRequest> requestList = [];
  List<GroupSummary> groupList = [];
  GroupDetail? groupDetail;
  ContactProfile? profileValue;

  int acceptCalls = 0;
  int rejectCalls = 0;
  final List<String> acceptKeys = [];
  Object? acceptError;
  Object? groupError;
  Object? profileSaveError;
  int groupLoads = 0;

  final List<Completer<PageResult<Contact>>> searches = [];

  @override
  Future<PageResult<Contact>> friends({
    String cursor = '',
    int limit = 50,
  }) async {
    if (friendPages.isEmpty) return PageResult.empty();
    final page = friendPages[friendPage.clamp(0, friendPages.length - 1)];
    friendPage++;
    return page;
  }

  @override
  Future<PageResult<Contact>> search(
    String query, {
    String cursor = '',
    int limit = 50,
  }) {
    final completer = Completer<PageResult<Contact>>();
    searches.add(completer);
    return completer.future;
  }

  @override
  Future<PageResult<FriendRequest>> requests({
    String cursor = '',
    int limit = 50,
  }) async => PageResult(items: List.of(requestList), hasMore: false);

  @override
  Future<FriendRequest> acceptRequest(
    String requestId, {
    required String idempotencyKey,
  }) async {
    acceptCalls++;
    acceptKeys.add(idempotencyKey);
    if (acceptError != null) throw acceptError!;
    requestList = [
      for (final item in requestList)
        if (item.requestId == requestId)
          request(requestId, status: FriendRequestStatus.accepted)
        else
          item,
    ];
    return request(requestId, status: FriendRequestStatus.accepted);
  }

  @override
  Future<FriendRequest> rejectRequest(
    String requestId, {
    required String idempotencyKey,
  }) async {
    rejectCalls++;
    return request(requestId, status: FriendRequestStatus.rejected);
  }

  @override
  Future<void> cancelRequest(
    String requestId, {
    required String idempotencyKey,
  }) async {}

  @override
  Future<ContactProfile> profile(String userId) async =>
      profileValue ??
      ContactProfile(
        userId: userId,
        displayName: userId,
        isSelf: false,
        isFriend: true,
      );

  @override
  Future<ContactProfile> updatePrefs(
    String userId, {
    required int expectedVersion,
    String? remark,
    bool? starred,
    List<String>? tags,
  }) async {
    if (profileSaveError != null) throw profileSaveError!;
    final current = profileValue!;
    profileValue = ContactProfile(
      userId: current.userId,
      displayName: current.displayName,
      isSelf: current.isSelf,
      remark: remark ?? current.remark,
      starred: starred ?? current.starred,
      tags: tags ?? current.tags,
      isFriend: current.isFriend,
      version: current.version + 1,
    );
    return profileValue!;
  }

  @override
  Future<ContactProfile> block(
    String userId, {
    required String idempotencyKey,
  }) async => profileValue = ContactProfile(
    userId: userId,
    displayName: userId,
    isSelf: false,
    blocked: true,
    isFriend: true,
  );

  @override
  Future<ContactProfile> unblock(
    String userId, {
    required String idempotencyKey,
  }) async => profileValue = ContactProfile(
    userId: userId,
    displayName: userId,
    isSelf: false,
    isFriend: true,
  );

  @override
  Future<void> removeFriend(
    String userId, {
    required String idempotencyKey,
  }) async {}

  @override
  Future<PageResult<GroupSummary>> groups({
    String cursor = '',
    int limit = 50,
  }) async => PageResult(items: List.of(groupList), hasMore: false);

  @override
  Future<GroupDetail> group(String groupId) async {
    groupLoads++;
    if (groupError != null) throw groupError!;
    final detail = groupDetail!;
    return GroupDetail(
      summary: detail.summary,
      members: detail.members,
      myRole: detail.myRole,
    );
  }

  @override
  Future<GroupDetail> addMembers(
    String groupId,
    List<String> members, {
    required int expectedVersion,
    required String idempotencyKey,
  }) async {
    if (groupError != null) throw groupError!;
    return groupDetail!;
  }

  @override
  Future<GroupDetail> changeRole(
    String groupId,
    String userId,
    GroupRole role, {
    required int expectedVersion,
    required String idempotencyKey,
  }) async {
    if (groupError != null) throw groupError!;
    return groupDetail!;
  }

  @override
  Future<void> kick(
    String groupId,
    String userId, {
    required int expectedVersion,
    required String idempotencyKey,
  }) async {
    if (groupError != null) throw groupError!;
  }

  @override
  Future<void> leave(String groupId, {required String idempotencyKey}) async {}
}

void main() {
  group('ContactsController', () {
    test('分页追加并按 userId 去重', () async {
      final port = FakeRelations()
        ..friendPages = [
          PageResult(
            items: [contact('a'), contact('b')],
            hasMore: true,
            nextCursor: 'c2',
          ),
          PageResult(items: [contact('b'), contact('c')], hasMore: false),
        ];
      final controller = ContactsController(port: port);
      await controller.load();
      expect(controller.contacts.map((c) => c.userId), ['a', 'b']);
      expect(controller.hasMore, isTrue);

      await controller.loadMore();
      expect(controller.contacts.map((c) => c.userId), ['a', 'b', 'c']);
      expect(controller.hasMore, isFalse);
      controller.dispose();
    });

    test('搜索丢弃过期结果', () async {
      final port = FakeRelations();
      final controller = ContactsController(port: port);

      final first = controller.search('a');
      final second = controller.search('bo');
      expect(port.searches.length, 2);

      port.searches[1].complete(
        PageResult(items: [contact('bob'), contact('bonnie')], hasMore: false),
      );
      await second;
      port.searches[0].complete(
        PageResult(items: [contact('alice')], hasMore: false),
      );
      await first;

      expect(controller.contacts.map((c) => c.userId), ['bob', 'bonnie']);
      controller.dispose();
    });

    test('重复点击审批只提交一次，成功后重新查询', () async {
      final port = FakeRelations()
        ..requestList = [request('r1')]
        ..friendPages = [
          PageResult(items: [contact('a')], hasMore: false),
        ];
      final controller = ContactsController(port: port);
      await controller.loadRequests();

      final item = controller.requests.single;
      final first = controller.decide(item, accept: true);
      final second = controller.decide(item, accept: true);
      await Future.wait([first, second]);

      expect(port.acceptCalls, 1);
      expect(controller.requests.single.status, FriendRequestStatus.accepted);
      controller.dispose();
    });

    test('审批失败保留原始状态并暴露业务错误', () async {
      final port = FakeRelations()
        ..requestList = [request('r1')]
        ..acceptError = const G12Failure(
          G12FailureKind.forbidden,
          'FORBIDDEN',
          '已拉黑',
        );
      final controller = ContactsController(port: port);
      await controller.loadRequests();

      await controller.decide(controller.requests.single, accept: true);

      expect(controller.requests.single.status, FriendRequestStatus.pending);
      expect(controller.requestError?.kind, G12FailureKind.forbidden);
      controller.dispose();
    });
  });

  group('ProfileController', () {
    test('冲突时重新拉取版本，避免继续覆盖', () async {
      final port = FakeRelations()
        ..profileValue = const ContactProfile(
          userId: 'u1',
          displayName: '张三',
          isSelf: false,
          isFriend: true,
          version: 3,
        )
        ..profileSaveError = const G12Failure(
          G12FailureKind.conflict,
          'CONFLICT',
          '版本已变化',
        );
      final controller = ProfileController(port: port, userId: 'u1');
      await controller.load();

      expect(await controller.savePrefs(remark: '新备注'), isFalse);
      expect(controller.error?.kind, G12FailureKind.conflict);
      controller.dispose();
    });

    test('星标以服务端返回为准', () async {
      final port = FakeRelations()
        ..profileValue = const ContactProfile(
          userId: 'u1',
          displayName: '张三',
          isSelf: false,
          isFriend: true,
          version: 1,
        );
      final controller = ProfileController(port: port, userId: 'u1');
      await controller.load();
      expect(await controller.toggleStar(), isTrue);
      expect(controller.profile?.starred, isTrue);
      expect(controller.profile?.version, 2);
      controller.dispose();
    });

    test('自己的资料不发写请求', () async {
      final port = FakeRelations()
        ..profileValue = const ContactProfile(
          userId: 'me',
          displayName: '我',
          isSelf: true,
        );
      final controller = ProfileController(port: port, userId: 'me');
      await controller.load();
      expect(await controller.savePrefs(remark: 'x'), isFalse);
      expect(controller.profile?.remark, isEmpty);
      controller.dispose();
    });
  });

  group('GroupController', () {
    GroupDetail detail({GroupRole myRole = GroupRole.owner, int version = 1}) =>
        GroupDetail(
          summary: GroupSummary(
            groupId: 'g1',
            name: '项目组',
            memberCount: 3,
            role: myRole,
            version: version,
          ),
          members: const [
            GroupMember(userId: 'me', displayName: '我', role: GroupRole.owner),
            GroupMember(
              userId: 'u2',
              displayName: '张三',
              role: GroupRole.member,
            ),
          ],
          myRole: myRole,
        );

    test('越权操作显示业务拒绝，不改本地成员状态', () async {
      final port = FakeRelations()
        ..groupDetail = detail(myRole: GroupRole.member);
      final controller = GroupController(port: port, groupId: 'g1');
      await controller.load();
      port.groupError = const G12Failure(
        G12FailureKind.forbidden,
        'FORBIDDEN',
        '只有群主可以操作',
      );

      expect(await controller.kick('u2'), isFalse);
      expect(controller.error?.kind, G12FailureKind.forbidden);
      expect(controller.detail?.members.length, 2);
      controller.dispose();
    });

    test('版本冲突后自动重新加载', () async {
      final port = FakeRelations()..groupDetail = detail();
      final controller = GroupController(port: port, groupId: 'g1');
      await controller.load();
      final loadsBefore = port.groupLoads;
      port.groupError = const G12Failure(
        G12FailureKind.conflict,
        'CONFLICT',
        '成员已变化',
      );

      expect(await controller.changeRole('u2', GroupRole.admin), isFalse);
      expect(controller.error?.kind, G12FailureKind.conflict);
      expect(port.groupLoads, greaterThan(loadsBefore));
      controller.dispose();
    });
  });
}
