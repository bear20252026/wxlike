/// G13-05：搜一搜。
///
/// 搜索历史与结果都来自 `SearchPort`；没有查询词时不返回"热门推荐"之类的静态
/// 结果。结果里的 `available == false` 只保留占位，不提供打开入口。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

/// 搜索范围。空串表示全部，其余值原样作为 `type` 传给服务端。
const List<(String, String)> _types = [
  ('', '全部'),
  ('message', '聊天'),
  ('contact', '联系人'),
  ('article', '文章'),
  ('favorite', '收藏'),
];

IconData _iconFor(String type) => switch (type) {
  'message' => Icons.chat_bubble_outline,
  'contact' => Icons.person_outline,
  'article' => Icons.article_outlined,
  'favorite' => Icons.star_border,
  'moment' => Icons.photo_outlined,
  _ => Icons.search,
};

class SearchPage extends StatefulWidget {
  const SearchPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<SearchPage> createState() => _SearchPageState();
}

class _SearchPageState extends State<SearchPage> {
  final TextEditingController _controller = TextEditingController();

  List<String> _history = const [];
  Object? _historyError;
  String _clearKey = newIdempotencyKey();

  final List<SearchHit> _hits = [];
  String _query = '';
  String _type = '';
  String _cursor = '';
  bool _hasMore = false;
  bool _searched = false;
  bool _searching = false;
  bool _loadingMore = false;
  bool _clearingHistory = false;
  Object? _error;
  Object? _pageError;

  @override
  void initState() {
    super.initState();
    _loadHistory();
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _loadHistory() async {
    try {
      final history = await widget.ports.search.history();
      if (!mounted) return;
      setState(() {
        _history = history;
        _historyError = null;
      });
    } catch (error) {
      if (mounted) setState(() => _historyError = error);
    }
  }

  Future<void> _run(String rawQuery, {bool append = false}) async {
    final query = rawQuery.trim();
    if (query.isEmpty) {
      setState(() {
        _error = const G13Failure(
          G13FailureKind.invalidArgument,
          'EMPTY_QUERY',
          '请输入搜索内容',
        );
        _searched = false;
      });
      return;
    }
    if (append) {
      if (!_hasMore || _loadingMore || _searching) return;
      setState(() {
        _loadingMore = true;
        _pageError = null;
      });
    } else {
      if (_searching) return;
      setState(() {
        _searching = true;
        _error = null;
        _pageError = null;
        _query = query;
        _searched = true;
      });
    }
    try {
      final page = await widget.ports.search.search(
        query,
        type: _type,
        cursor: append ? _cursor : '',
      );
      if (!mounted) return;
      setState(() {
        if (append) {
          _hits.addAll(page.items);
        } else {
          _hits
            ..clear()
            ..addAll(page.items);
        }
        _cursor = page.nextCursor;
        _hasMore = page.hasMore && page.nextCursor.isNotEmpty;
      });
      if (!append) _loadHistory();
    } catch (error) {
      if (!mounted) return;
      setState(() {
        if (append) {
          _pageError = error;
        } else {
          _error = error;
          _hits.clear();
          _hasMore = false;
        }
      });
    } finally {
      if (mounted) {
        setState(() {
          _searching = false;
          _loadingMore = false;
        });
      }
    }
  }

  void _pickHistory(String query) {
    _controller.text = query;
    _controller.selection = TextSelection.collapsed(offset: query.length);
    _run(query);
  }

  Future<void> _clearHistory() async {
    if (_clearingHistory) return;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('清空搜索历史'),
        content: const Text('清空后不可恢复。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('清空'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    setState(() => _clearingHistory = true);
    try {
      await widget.ports.search.clearHistory(idempotencyKey: _clearKey);
      if (!mounted) return;
      setState(() {
        _history = const [];
        _historyError = null;
        _clearKey = newIdempotencyKey();
      });
    } catch (error) {
      if (!mounted) return;
      final text = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    } finally {
      if (mounted) setState(() => _clearingHistory = false);
    }
  }

  void _openHit(SearchHit hit) {
    if (!hit.available) return;
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.all(20),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(hit.title, style: Theme.of(context).textTheme.titleMedium),
              const SizedBox(height: 8),
              if (hit.snippet.isNotEmpty) SelectableText(hit.snippet),
              const SizedBox(height: 12),
              Text(
                '来源：${hit.source} · 类型：${hit.type}',
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '搜一搜',
      onBack: () => Navigator.of(context).maybePop(),
      body: Column(
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
            child: TextField(
              controller: _controller,
              textInputAction: TextInputAction.search,
              onSubmitted: (value) => _run(value),
              decoration: InputDecoration(
                hintText: '搜索聊天、联系人、文章、收藏',
                prefixIcon: const Icon(Icons.search),
                border: const OutlineInputBorder(),
                isDense: true,
                suffixIcon: IconButton(
                  onPressed: () {
                    _controller.clear();
                    setState(() {
                      _searched = false;
                      _hits.clear();
                      _error = null;
                      _pageError = null;
                      _query = '';
                    });
                  },
                  icon: const Icon(Icons.close),
                ),
              ),
            ),
          ),
          SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
            child: Row(
              children: [
                for (final (value, label) in _types)
                  Padding(
                    padding: const EdgeInsets.only(right: 8),
                    child: ChoiceChip(
                      label: Text(label),
                      selected: _type == value,
                      onSelected: _searching
                          ? null
                          : (_) {
                              setState(() => _type = value);
                              final current = _controller.text.trim();
                              _run(current.isEmpty ? _query : current);
                            },
                    ),
                  ),
              ],
            ),
          ),
          Expanded(child: _body()),
        ],
      ),
    );
  }

  Widget _body() {
    if (_searching && _hits.isEmpty) {
      return const Center(child: CircularProgressIndicator());
    }
    if (_error != null) {
      return FailureView(
        error: _error!,
        onRetry: _searched ? () => _run(_query) : null,
      );
    }
    if (!_searched) return _historyBody();
    if (_hits.isEmpty) {
      return EmptyView(
        text: '没有找到与“$_query”相关的内容',
        hint: _type.isEmpty ? '' : '试试切换搜索范围或换个关键词',
      );
    }
    return ListView.separated(
      itemCount: _hits.length + 1,
      separatorBuilder: (_, _) => const Divider(height: 1),
      itemBuilder: (context, index) {
        if (index == _hits.length) return _footer();
        final hit = _hits[index];
        return ListTile(
          leading: Icon(_iconFor(hit.type)),
          title: Text(hit.title, maxLines: 1, overflow: TextOverflow.ellipsis),
          subtitle: Text(
            hit.available ? hit.snippet : '内容已不可用',
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          ),
          trailing: hit.available
              ? const Icon(Icons.chevron_right)
              : const Pill(text: '不可用'),
          onTap: hit.available ? () => _openHit(hit) : null,
        );
      },
    );
  }

  Widget _footer() {
    if (_pageError != null) {
      return InlineFailure(
        error: _pageError!,
        onRetry: _loadingMore ? null : () => _run(_query, append: true),
      );
    }
    if (!_hasMore) return const SizedBox(height: 24);
    return Padding(
      padding: const EdgeInsets.all(12),
      child: Center(
        child: _loadingMore
            ? const CircularProgressIndicator()
            : TextButton(
                onPressed: () => _run(_query, append: true),
                child: const Text('加载更多'),
              ),
      ),
    );
  }

  Widget _historyBody() {
    if (_historyError != null) {
      return FailureView(error: _historyError!, onRetry: _loadHistory);
    }
    if (_history.isEmpty) {
      return const EmptyView(text: '还没有搜索记录', hint: '输入关键词开始搜索');
    }
    return ListView(
      children: [
        SectionTitle(
          text: '搜索历史',
          trailing: TextButton(
            onPressed: _clearingHistory ? null : _clearHistory,
            child: const Text('清空'),
          ),
        ),
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16),
          child: Wrap(
            spacing: 8,
            runSpacing: 4,
            children: [
              for (final item in _history)
                ActionChip(
                  label: Text(item),
                  onPressed: () => _pickHistory(item),
                ),
            ],
          ),
        ),
        const SizedBox(height: 24),
      ],
    );
  }
}
