// Minimal lint (P3): typescript-eslint recommended + a no-unused-vars tuned
// for the codebase's `_`-prefixed intentionally-unused params (`_to`, `_convId`).
// Vendored tweb code (GPL, third-party) and dist output are out of scope.
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist/**", "node_modules/**", "src/vendor/**"] },
  ...tseslint.configs.recommended.map((c) => ({ ...c, files: ["src/**/*.ts"] })),
  {
    files: ["src/**/*.ts"],
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_", caughtErrors: "none" },
      ],
    },
  },
);
