/// G13-01：公众号、订阅号与文章阅读。
///
/// 文章正文按 `docVersion` 取：列表里的卡片和正文可能不是同一版，服务端拒绝
/// 时页面提示刷新，而不是把旧正文贴在新标题下。阅读页离页时取消订阅，避免
/// 后台继续拉取。
library;

import 'package:flutter/material.dart';

import 'g13/core.dart';
import 'g13/models.dart';
import 'g13/ports.dart';
import 'g13/widgets.dart';

class PublisherListPage extends StatefulWidget {
  const PublisherListPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<PublisherListPage> createState() => _PublisherListPageState();
}

class _PublisherListPageState extends State<PublisherListPage> {
  late Future<PageResult<Publisher>> _future = widget.ports.publisher
      .publishers();
  final Set<String> _busy = <String>{};

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.publisher.publishers();
    });
  }

  void _report(Object error) {
    if (!mounted) return;
    final text = error is G13Failure ? error.displayText : '$error';
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
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
            'sub-${publisher.id}-${!publisher.subscribed}-${publisher.version}',
      );
      _reload();
    } catch (error) {
      _report(error);
    } finally {
      if (mounted) setState(() => _busy.remove(publisher.id));
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '订阅号消息',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
      ],
      body: AsyncView<PageResult<Publisher>>(
        future: _future,
        onRetry: _reload,
        isEmpty: (page) => page.isEmpty,
        emptyText: '还没有关注公众号',
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
                publisher.description.isEmpty ? '暂无简介' : publisher.description,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
              trailing: _busy.contains(publisher.id)
                  ? const SizedBox(
                      width: 18,
                      height: 18,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : TextButton(
                      onPressed: () => _toggle(publisher),
                      child: Text(publisher.subscribed ? '已关注' : '关注'),
                    ),
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(
                  builder: (_) => PublisherArticlesPage(
                    ports: widget.ports,
                    publisher: publisher,
                  ),
                ),
              ),
            );
          },
        ),
      ),
    );
  }
}

class PublisherArticlesPage extends StatefulWidget {
  const PublisherArticlesPage({
    super.key,
    required this.ports,
    required this.publisher,
  });

  final G13Ports ports;
  final Publisher publisher;

  @override
  State<PublisherArticlesPage> createState() => _PublisherArticlesPageState();
}

class _PublisherArticlesPageState extends State<PublisherArticlesPage> {
  late Future<PageResult<Article>> _future = widget.ports.publisher.articles(
    widget.publisher.id,
  );

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.publisher.articles(widget.publisher.id);
    });
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: widget.publisher.name,
      subtitle: widget.publisher.subscribed ? '已关注' : '未关注',
      onBack: () => Navigator.of(context).maybePop(),
      body: AsyncView<PageResult<Article>>(
        future: _future,
        onRetry: _reload,
        isEmpty: (page) => page.isEmpty,
        emptyText: '这个公众号还没有文章',
        builder: (context, page) => ListView.separated(
          itemCount: page.items.length,
          separatorBuilder: (_, _) => const Divider(height: 1),
          itemBuilder: (context, index) {
            final article = page.items[index];
            return ListTile(
              title: Text(
                article.title,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
              ),
              subtitle: Text(
                article.revoked
                    ? '内容已撤权，无法打开'
                    : '${formatTime(article.publishedAt)} · ${article.summary}',
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
              ),
              trailing: article.revoked
                  ? const Pill(text: '已撤权')
                  : const Icon(Icons.chevron_right),
              enabled: !article.revoked,
              onTap: article.revoked
                  ? null
                  : () => Navigator.of(context).push(
                      MaterialPageRoute<void>(
                        builder: (_) =>
                            ArticlePage(ports: widget.ports, article: article),
                      ),
                    ),
            );
          },
        ),
      ),
    );
  }
}

class ArticlePage extends StatefulWidget {
  const ArticlePage({super.key, required this.ports, required this.article});

  final G13Ports ports;
  final Article article;

  @override
  State<ArticlePage> createState() => _ArticlePageState();
}

class _ArticlePageState extends State<ArticlePage> with SubmitGuard {
  late Future<Article> _future = widget.ports.publisher.article(
    widget.article.id,
    docVersion: widget.article.docVersion,
  );

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.publisher.article(
        widget.article.id,
        docVersion: widget.article.docVersion,
      );
    });
  }

  Future<void> _favorite() async {
    await guard(() async {
      try {
        await widget.ports.content.addFavorite(
          type: 'article',
          sourceId: widget.article.id,
          note: '',
          idempotencyKey:
              'fav-article-${widget.article.id}-${widget.article.docVersion}',
        );
        if (!mounted) return;
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('已收藏')));
      } catch (error) {
        if (!mounted) return;
        final text = error is G13Failure ? error.displayText : '$error';
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(text)));
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '文章',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(
          tooltip: '收藏',
          onPressed: submitting ? null : _favorite,
          icon: const Icon(Icons.star_border),
        ),
      ],
      body: AsyncView<Article>(
        future: _future,
        onRetry: _reload,
        builder: (context, article) {
          if (article.revoked) {
            return const EmptyView(text: '内容已撤权', hint: '源发布者撤回了这篇文章，正文不再提供。');
          }
          return ListView(
            padding: const EdgeInsets.all(16),
            children: [
              Text(
                article.title,
                style: Theme.of(context).textTheme.headlineSmall,
              ),
              const SizedBox(height: 8),
              Text(
                '${article.author} · ${formatTime(article.publishedAt)}',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 16),
              for (final block in article.blocks)
                _ArticleBlockView(block: block),
            ],
          );
        },
      ),
    );
  }
}

class _ArticleBlockView extends StatelessWidget {
  const _ArticleBlockView({required this.block});

  final ArticleBlock block;

  @override
  Widget build(BuildContext context) {
    switch (block.kind) {
      case 'heading':
        return Padding(
          padding: const EdgeInsets.only(top: 12, bottom: 6),
          child: Text(
            block.text,
            style: Theme.of(context).textTheme.titleLarge,
          ),
        );
      case 'image':
        return Padding(
          padding: const EdgeInsets.symmetric(vertical: 8),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Container(
                height: 160,
                alignment: Alignment.center,
                decoration: BoxDecoration(
                  color: Theme.of(context).colorScheme.surfaceContainerHighest,
                  borderRadius: BorderRadius.circular(8),
                ),
                child: const Icon(Icons.image_outlined),
              ),
              if (block.caption.isNotEmpty)
                Text(
                  block.caption,
                  style: Theme.of(context).textTheme.bodySmall,
                ),
            ],
          ),
        );
      default:
        return Padding(
          padding: const EdgeInsets.symmetric(vertical: 6),
          child: Text(block.text, style: const TextStyle(height: 1.6)),
        );
    }
  }
}

/// 订阅号文章列表里的未读角标需要预读一次公众号数据；这里给导航层一个
/// 统一的入口，避免调用方自己拼 route。
Future<void> openPublisherList(BuildContext context, G13Ports ports) {
  return Navigator.of(context).push(
    MaterialPageRoute<void>(builder: (_) => PublisherListPage(ports: ports)),
  );
}
