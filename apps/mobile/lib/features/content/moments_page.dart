/// G13-01：朋友圈。
///
/// 发布、点赞、评论、删除都走真实 `content.*` 端口；每次写操作带幂等键，重复
/// 提交由服务端去重。列表刷新以服务端快照为准，不在本地先改计数再"看起来成功"。
library;

import 'package:flutter/material.dart';

import 'g13/core.dart';
import 'g13/models.dart';
import 'g13/ports.dart';
import 'g13/widgets.dart';

class MomentsPage extends StatefulWidget {
  const MomentsPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<MomentsPage> createState() => _MomentsPageState();
}

class _MomentsPageState extends State<MomentsPage> with SubmitGuard {
  late Future<PageResult<Moment>> _feed = _load();
  final Set<String> _busy = <String>{};

  /// 一次发布意图只生成一次键：对话框里改文案不算新的意图，服务端按同一
  /// 幂等键返回同一条动态。
  String _publishKey = newIdempotencyKey();

  Future<PageResult<Moment>> _load() => widget.ports.content.moments();

  void _reload() {
    if (!mounted) return;
    setState(() {
      _feed = _load();
    });
  }

  void _report(Object error) {
    if (!mounted) return;
    final text = error is G13Failure ? error.displayText : '$error';
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _toggleLike(Moment moment) async {
    if (_busy.contains(moment.id)) return;
    setState(() => _busy.add(moment.id));
    try {
      await widget.ports.content.setMomentLike(
        moment.id,
        !moment.liked,
        idempotencyKey: 'like-${moment.id}-${!moment.liked}',
      );
      _reload();
    } catch (error) {
      _report(error);
    } finally {
      if (mounted) setState(() => _busy.remove(moment.id));
    }
  }

  Future<void> _comment(Moment moment) async {
    final text = await showDialog<String>(
      context: context,
      builder: (context) =>
          _TextPromptDialog(title: '评论 ${moment.authorName}', hint: '说点什么'),
    );
    if (text == null || text.trim().isEmpty) return;
    final body = text.trim();
    try {
      await widget.ports.content.commentMoment(
        moment.id,
        body,
        idempotencyKey: 'comment-${moment.id}-${body.hashCode}',
      );
      _reload();
    } catch (error) {
      _report(error);
    }
  }

  Future<void> _compose() async {
    final draft = await showDialog<_ComposeResult>(
      context: context,
      builder: (context) => const _ComposeMomentDialog(),
    );
    if (draft == null) return;
    await guard(() async {
      try {
        await widget.ports.content.publishMoment(
          MomentDraft(
            text: draft.text,
            mediaIds: draft.mediaIds,
            audience: draft.audience,
          ),
          idempotencyKey: _publishKey,
        );
        _publishKey = newIdempotencyKey();
        _reload();
      } catch (error) {
        _report(error);
      }
    });
  }

  Future<void> _delete(Moment moment) async {
    final confirmed = await _confirm(context, '删除这条动态？', '删除后好友侧也会消失。');
    if (!confirmed) return;
    try {
      await widget.ports.content.deleteMoment(
        moment.id,
        idempotencyKey: 'delete-${moment.id}',
      );
      _reload();
    } catch (error) {
      _report(error);
    }
  }

  Future<void> _favorite(Moment moment) async {
    try {
      await widget.ports.content.addFavorite(
        type: 'moment',
        sourceId: moment.id,
        note: '',
        idempotencyKey: 'fav-moment-${moment.id}',
      );
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('已加入收藏')));
      }
    } catch (error) {
      _report(error);
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '朋友圈',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(
          tooltip: '刷新',
          onPressed: _reload,
          icon: const Icon(Icons.refresh),
        ),
      ],
      floatingActionButton: FloatingActionButton(
        onPressed: submitting ? null : _compose,
        child: submitting
            ? const SizedBox(
                width: 18,
                height: 18,
                child: CircularProgressIndicator(strokeWidth: 2),
              )
            : const Icon(Icons.edit),
      ),
      body: AsyncView<PageResult<Moment>>(
        future: _feed,
        onRetry: _reload,
        isEmpty: (page) => page.isEmpty,
        emptyText: '还没有动态',
        emptyHint: '发布第一条，好友刷新后可见',
        builder: (context, page) => RefreshIndicator(
          onRefresh: () async => _reload(),
          child: ListView.separated(
            physics: const AlwaysScrollableScrollPhysics(),
            padding: const EdgeInsets.only(bottom: 96),
            itemCount: page.items.length,
            separatorBuilder: (_, _) => const Divider(height: 1),
            itemBuilder: (context, index) {
              final moment = page.items[index];
              return _MomentTile(
                moment: moment,
                busy: _busy.contains(moment.id),
                onLike: () => _toggleLike(moment),
                onComment: () => _comment(moment),
                onFavorite: () => _favorite(moment),
                onDelete: moment.isMine ? () => _delete(moment) : null,
              );
            },
          ),
        ),
      ),
    );
  }
}

class _MomentTile extends StatelessWidget {
  const _MomentTile({
    required this.moment,
    required this.busy,
    required this.onLike,
    required this.onComment,
    required this.onFavorite,
    this.onDelete,
  });

  final Moment moment;
  final bool busy;
  final VoidCallback onLike;
  final VoidCallback onComment;
  final VoidCallback onFavorite;
  final VoidCallback? onDelete;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 12, 16, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              AvatarDot(label: moment.authorName),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      moment.authorName,
                      style: Theme.of(context).textTheme.titleSmall,
                    ),
                    Text(
                      '${formatTime(moment.createdAt)} · ${moment.audience == 'all' ? '公开' : '好友可见'}',
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ],
                ),
              ),
              if (onDelete != null)
                PopupMenuButton<String>(
                  onSelected: (value) {
                    if (value == 'delete') onDelete!();
                  },
                  itemBuilder: (context) => const [
                    PopupMenuItem(value: 'delete', child: Text('删除')),
                  ],
                ),
            ],
          ),
          if (moment.text.isNotEmpty) ...[
            const SizedBox(height: 8),
            Text(moment.text),
          ],
          if (moment.media.isNotEmpty) ...[
            const SizedBox(height: 8),
            Wrap(
              spacing: 6,
              runSpacing: 6,
              children: [
                for (final media in moment.media)
                  Container(
                    width: 76,
                    height: 76,
                    decoration: BoxDecoration(
                      color: Theme.of(context)
                          .colorScheme
                          .surfaceContainerHighest,
                      borderRadius: BorderRadius.circular(6),
                    ),
                    alignment: Alignment.center,
                    child: media.mediaId.isEmpty
                        ? const Icon(Icons.broken_image_outlined)
                        : const Icon(Icons.image_outlined),
                  ),
              ],
            ),
          ],
          const SizedBox(height: 4),
          Row(
            children: [
              TextButton.icon(
                onPressed: busy ? null : onLike,
                icon: Icon(
                  moment.liked ? Icons.favorite : Icons.favorite_border,
                  size: 18,
                  color: moment.liked ? Colors.redAccent : null,
                ),
                label: Text('${moment.likeCount}'),
              ),
              TextButton.icon(
                onPressed: onComment,
                icon: const Icon(Icons.chat_bubble_outline, size: 18),
                label: const Text('评论'),
              ),
              TextButton.icon(
                onPressed: onFavorite,
                icon: const Icon(Icons.star_border, size: 18),
                label: const Text('收藏'),
              ),
            ],
          ),
          if (moment.comments.isNotEmpty)
            Container(
              width: double.infinity,
              padding: const EdgeInsets.all(8),
              decoration: BoxDecoration(
                color: Theme.of(context).colorScheme.surfaceContainerHighest,
                borderRadius: BorderRadius.circular(6),
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  for (final comment in moment.comments)
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 2),
                      child: Text('${comment.authorName}：${comment.text}'),
                    ),
                ],
              ),
            ),
        ],
      ),
    );
  }
}

class _ComposeResult {
  const _ComposeResult({
    required this.text,
    required this.audience,
    required this.mediaIds,
  });

  final String text;
  final String audience;
  final List<String> mediaIds;
}

class _ComposeMomentDialog extends StatefulWidget {
  const _ComposeMomentDialog();

  @override
  State<_ComposeMomentDialog> createState() => _ComposeMomentDialogState();
}

class _ComposeMomentDialogState extends State<_ComposeMomentDialog> {
  final TextEditingController _text = TextEditingController();
  String _audience = 'friends';

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('发表动态'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            controller: _text,
            maxLines: 5,
            maxLength: 1000,
            decoration: const InputDecoration(hintText: '这一刻的想法…'),
          ),
          const SizedBox(height: 8),
          SegmentedButton<String>(
            segments: const [
              ButtonSegment(value: 'friends', label: Text('好友可见')),
              ButtonSegment(value: 'private', label: Text('仅自己')),
            ],
            selected: {_audience},
            onSelectionChanged: (value) =>
                setState(() => _audience = value.first),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () {
            final text = _text.text.trim();
            if (text.isEmpty) return;
            Navigator.of(context).pop(
              _ComposeResult(
                text: text,
                audience: _audience,
                mediaIds: const [],
              ),
            );
          },
          child: const Text('发布'),
        ),
      ],
    );
  }
}

class _TextPromptDialog extends StatefulWidget {
  const _TextPromptDialog({required this.title, required this.hint});

  final String title;
  final String hint;

  @override
  State<_TextPromptDialog> createState() => _TextPromptDialogState();
}

class _TextPromptDialogState extends State<_TextPromptDialog> {
  final TextEditingController _text = TextEditingController();

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: Text(widget.title),
      content: TextField(
        controller: _text,
        autofocus: true,
        maxLength: 200,
        decoration: InputDecoration(hintText: widget.hint),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(_text.text),
          child: const Text('发送'),
        ),
      ],
    );
  }
}

Future<bool> _confirm(BuildContext context, String title, String body) async {
  final result = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(title),
      content: Text(body),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(true),
          child: const Text('确定'),
        ),
      ],
    ),
  );
  return result ?? false;
}
