/// UI v2 的设计 token。
///
/// 颜色与尺寸取自 `design/ui-v2/prototype`（微信浅色基线），深浅色各一套。
/// 页面只引用这里的常量，不自己写十六进制，避免 27 个手机页各有一套"绿"。
library;

import 'package:flutter/material.dart';

class WxColors {
  const WxColors._();

  static const brand = Color(0xff07C160);
  static const brandDark = Color(0xff06AD56);
  static const brandSoft = Color(0xffE9F8EF);
  static const link = Color(0xff576B95);
  static const danger = Color(0xffFA5151);
  static const warning = Color(0xffF5B640);

  static const bg = Color(0xffEDEDED);
  static const bgGrouped = Color(0xffF5F5F5);
  static const surface = Color(0xffFFFFFF);
  static const overlay = Color(0xffF7F7F7);

  static const text = Color(0xff1A1A1A);
  static const textSecondary = Color(0xff9A9A9A);
  static const textTertiary = Color(0xffB5B5B5);
  static const divider = Color(0xffEFEFEF);
  static const chatMine = Color(0xff95EC69);

  static const darkBg = Color(0xff111111);
  static const darkSurface = Color(0xff1C1C1E);
  static const darkGrouped = Color(0xff161616);
  static const darkText = Color(0xffEDEDED);
  static const darkTextSecondary = Color(0xff9A9A9A);
  static const darkDivider = Color(0xff2C2C2E);
  static const darkChatMine = Color(0xff3A6B2B);
}

class WxRadius {
  const WxRadius._();

  static const tile = 8.0;
  static const card = 12.0;
  static const sheet = 20.0;
  static const bubble = 5.0;
}

class WxTheme {
  const WxTheme._();

  static ThemeData light() => _build(Brightness.light);

  static ThemeData dark() => _build(Brightness.dark);

  static ThemeData _build(Brightness brightness) {
    final dark = brightness == Brightness.dark;
    final scheme = ColorScheme.fromSeed(
      seedColor: WxColors.brand,
      brightness: brightness,
    ).copyWith(
      primary: WxColors.brand,
      onPrimary: Colors.white,
      secondary: WxColors.brandDark,
      error: WxColors.danger,
      surface: dark ? WxColors.darkSurface : WxColors.surface,
      onSurface: dark ? WxColors.darkText : WxColors.text,
    );
    final base = ThemeData(
      useMaterial3: true,
      colorScheme: scheme,
      scaffoldBackgroundColor: dark ? WxColors.darkBg : WxColors.bg,
      dividerColor: dark ? WxColors.darkDivider : WxColors.divider,
      visualDensity: VisualDensity.standard,
    );
    return base.copyWith(
      appBarTheme: AppBarTheme(
        backgroundColor: dark ? WxColors.darkBg : WxColors.bg,
        foregroundColor: dark ? WxColors.darkText : WxColors.text,
        elevation: 0,
        scrolledUnderElevation: 0.5,
        centerTitle: true,
        titleTextStyle: TextStyle(
          color: dark ? WxColors.darkText : WxColors.text,
          fontSize: 16,
          fontWeight: FontWeight.w600,
        ),
      ),
      bottomNavigationBarTheme: BottomNavigationBarThemeData(
        backgroundColor: dark ? WxColors.darkSurface : WxColors.overlay,
        selectedItemColor: WxColors.brand,
        unselectedItemColor: dark ? WxColors.darkTextSecondary : Colors.black54,
        type: BottomNavigationBarType.fixed,
        elevation: 0,
        showUnselectedLabels: true,
      ),
      inputDecorationTheme: InputDecorationTheme(
        isDense: true,
        filled: true,
        fillColor: dark ? WxColors.darkSurface : WxColors.surface,
        border: OutlineInputBorder(
          borderRadius: BorderRadius.circular(WxRadius.tile),
          borderSide: BorderSide(color: dark ? WxColors.darkDivider : WxColors.divider),
        ),
        enabledBorder: OutlineInputBorder(
          borderRadius: BorderRadius.circular(WxRadius.tile),
          borderSide: BorderSide(color: dark ? WxColors.darkDivider : WxColors.divider),
        ),
        focusedBorder: OutlineInputBorder(
          borderRadius: BorderRadius.circular(WxRadius.tile),
          borderSide: const BorderSide(color: WxColors.brand, width: 1.2),
        ),
      ),
      filledButtonTheme: FilledButtonThemeData(
        style: FilledButton.styleFrom(
          backgroundColor: WxColors.brand,
          foregroundColor: Colors.white,
          minimumSize: const Size(64, 44),
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(WxRadius.tile)),
        ),
      ),
      listTileTheme: ListTileThemeData(
        iconColor: dark ? WxColors.darkTextSecondary : Colors.black54,
        textColor: dark ? WxColors.darkText : WxColors.text,
      ),
      snackBarTheme: const SnackBarThemeData(behavior: SnackBarBehavior.floating),
    );
  }
}
