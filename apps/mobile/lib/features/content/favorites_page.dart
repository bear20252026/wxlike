/// G13-01：收藏。
///
/// 源内容撤权后条目仍然存在（用户自己收藏的东西不会凭空消失），但打开入口
/// 关闭并说明原因；删除走真实 `favorite.remove`，重复点击用同一幂等键。
library;

import 'package:flutter/material.dart';

import 'g13/core.dart';
import 'g13/models.dart';
import 'g13/ports.dart';
import 'g13/widgets.dart';

class FavoritesPage extends StatefulWidget {
  const FavoritesPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<FavoritesPage> createState() => _FavoritesPageState();
}

class _FavoritesPageState extends State<FavoritesPage> {
  static const _types = <String, String>{
    '': '全部',
    'article': '文章',
    'moment': '动态',
    'file': '文件',
    'link': '链接',
    'note': '笔记',
  };

  String _type = '';
  late Future<PageResult<FavoriteItem>> _future = _load();

  Future<PageResult<FavoriteItem>> _load() =>
      widget.ports.content.favorites(type: _type);

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = _load();
    });
  }

  void _report(Object error) {
    if (!mounted) return;
    final text = error is G13Failure ? error.displayText : '$error';
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _remove(FavoriteItem item) async {
    try {
      await widget.ports.content.removeFavorite(
        item.id,
        idempotencyKey: 'unfav-${item.id}',
      );
      _reload();
    } catch (error) {
      _report(error);
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '收藏',
      onBack: () => Navigator.of(context).maybePop(),
      body: Column(
        children: [
          SizedBox(
            height: 48,
            child: ListView.separated(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: 12),
              itemCount: _types.length,
              separatorBuilder: (_, _) => const SizedBox(width: 8),
              itemBuilder: (context, index) {
                final entry = _types.entries.elementAt(index);
                return Center(
                  child: ChoiceChip(
                    label: Text(entry.value),
                    selected: _type == entry.key,
                    onSelected: (_) {
                      setState(() {
                        _type = entry.key;
                        _future = _load();
                      });
                    },
                  ),
                );
              },
            ),
          ),
          const Divider(height: 1),
          Expanded(
            child: AsyncView<PageResult<FavoriteItem>>(
              future: _future,
              onRetry: _reload,
              isEmpty: (page) => page.isEmpty,
              emptyText: '这里还没有收藏',
              emptyHint: '在文章或动态里点收藏会出现在这里',
              builder: (context, page) => ListView.separated(
                itemCount: page.items.length,
                separatorBuilder: (_, _) => const Divider(height: 1),
                itemBuilder: (context, index) {
                  final item = page.items[index];
                  return ListTile(
                    leading: Icon(_iconFor(item.type)),
                    title: Text(
                      item.title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                    subtitle: Text(
                      item.available
                          ? '${item.source} · ${item.snippet}'
                          : '源内容已撤权',
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                    ),
                    trailing: PopupMenuButton<String>(
                      onSelected: (value) {
                        if (value == 'remove') _remove(item);
                      },
                      itemBuilder: (context) => const [
                        PopupMenuItem(value: 'remove', child: Text('取消收藏')),
                      ],
                    ),
                    onTap: item.available
                        ? () => showModalBottomSheet<void>(
                            context: context,
                            showDragHandle: true,
                            builder: (context) => _FavoriteDetail(item: item),
                          )
                        : null,
                  );
                },
              ),
            ),
          ),
        ],
      ),
    );
  }

  static IconData _iconFor(String type) => switch (type) {
    'article' => Icons.article_outlined,
    'moment' => Icons.photo_library_outlined,
    'file' => Icons.insert_drive_file_outlined,
    'link' => Icons.link,
    'note' => Icons.sticky_note_2_outlined,
    _ => Icons.bookmark_border,
  };
}

class _FavoriteDetail extends StatelessWidget {
  const _FavoriteDetail({required this.item});

  final FavoriteItem item;

  @override
  Widget build(BuildContext context) {
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(20, 0, 20, 24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(item.title, style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 6),
            Text(
              '${item.source} · ${formatTime(item.savedAt)}',
              style: Theme.of(context).textTheme.bodySmall,
            ),
            const SizedBox(height: 12),
            Text(item.snippet),
          ],
        ),
      ),
    );
  }
}

/// 从聊天/文章菜单进入收藏列表的统一入口。
Future<void> openFavorites(BuildContext context, G13Ports ports) {
  return Navigator.of(
    context,
  ).push(MaterialPageRoute<void>(builder: (_) => FavoritesPage(ports: ports)));
}
