/// G13-06：订阅号（公众号）整合入口。
///
/// 搜索走服务端 `publisher.list(query)`，不在本地过滤；每条文章卡由列表返回的
/// `publisher_id`/`article_id`/`docVersion` 打开各自正文，不共用样例文章。
library;

import 'package:flutter/material.dart';

import '../g13/core.dart';
import '../g13/models.dart';
import '../g13/ports.dart';
import '../g13/widgets.dart';
import '../reading_page.dart';

class OfficialPage extends StatefulWidget {
  const OfficialPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<OfficialPage> createState() => _OfficialPageState();
}

class _OfficialPageState extends State<OfficialPage> {
  final TextEditingController _search = TextEditingController();
  late Future<PageResult<Publisher>> _future = widget.ports.publisher
      .publishers();
  final Set<String> _busy = <String>{};

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.publisher.publishers(query: _search.text.trim());
    });
  }

  Future<void> _toggle(Publisher publisher) async {
    if (!_busy.add(publisher.id)) return;
    setState(() {});
    try {
      await widget.ports.publisher.setSubscribed(
        publisher.id,
        !publisher.subscribed,
        expectedVersion: publisher.version,
        idempotencyKey:
            'publisher-${publisher.id}-${!publisher.subscribed}-${publisher.version}',
      );
      _reload();
    } catch (error) {
      if (!mounted) return;
      final text = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    } finally {
      if (mounted) setState(() => _busy.remove(publisher.id));
    }
  }

  void _open(Publisher publisher) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) =>
            PublisherArticlesPage(ports: widget.ports, publisher: publisher),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final query = _search.text.trim();
    return G13Scaffold(
      title: '公众号',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
      ],
      body: Column(
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 12, 16, 8),
            child: TextField(
              controller: _search,
              textInputAction: TextInputAction.search,
              onSubmitted: (_) => _reload(),
              decoration: InputDecoration(
                hintText: '搜索公众号',
                prefixIcon: const Icon(Icons.search),
                border: const OutlineInputBorder(),
                isDense: true,
                suffixIcon: IconButton(
                  onPressed: () {
                    _search.clear();
                    _reload();
                  },
                  icon: const Icon(Icons.close),
                ),
              ),
            ),
          ),
          Expanded(
            child: AsyncView<PageResult<Publisher>>(
              future: _future,
              onRetry: _reload,
              isEmpty: (page) => page.isEmpty,
              emptyText: query.isEmpty ? '还没有关注公众号' : '没有找到与“$query”相关的公众号',
              emptyHint: query.isEmpty ? '搜索并添加感兴趣的公众号' : '换个关键词再试',
              builder: (context, page) => ListView.separated(
                itemCount: page.items.length,
                separatorBuilder: (_, _) => const Divider(height: 1),
                itemBuilder: (context, index) {
                  final publisher = page.items[index];
                  return ListTile(
                    leading: AvatarDot(
                      label: publisher.name,
                      color: const Color(0xff8d7cdf),
                    ),
                    title: Row(
                      children: [
                        Flexible(
                          child: Text(
                            publisher.name,
                            overflow: TextOverflow.ellipsis,
                          ),
                        ),
                        if (publisher.unread > 0) ...[
                          const SizedBox(width: 6),
                          Pill(text: '${publisher.unread} 条未读'),
                        ],
                      ],
                    ),
                    subtitle: Text(
                      publisher.description.isEmpty
                          ? '暂无简介'
                          : publisher.description,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                    trailing: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        if (_busy.contains(publisher.id))
                          const SizedBox(
                            width: 18,
                            height: 18,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        else
                          TextButton(
                            onPressed: () => _toggle(publisher),
                            child: Text(publisher.subscribed ? '已关注' : '添加'),
                          ),
                        PopupMenuButton<String>(
                          onSelected: (value) {
                            if (value == 'articles') _open(publisher);
                            if (value == 'toggle') _toggle(publisher);
                          },
                          itemBuilder: (context) => [
                            const PopupMenuItem<String>(
                              value: 'articles',
                              child: Text('查看文章'),
                            ),
                            PopupMenuItem<String>(
                              value: 'toggle',
                              child: Text(publisher.subscribed ? '取消关注' : '关注'),
                            ),
                          ],
                        ),
                      ],
                    ),
                    onTap: () => _open(publisher),
                  );
                },
              ),
            ),
          ),
        ],
      ),
    );
  }
}
