import js from '@eslint/js';
import globals from 'globals';
import prettierConfig from 'eslint-config-prettier';

export default [
    js.configs.recommended,
    {
        files: ['**/*.js'],
        languageOptions: {
            sourceType: 'module',
            ecmaVersion: 2024,
            globals: {
                ...globals.es2024,
                // GJS globals
                globalThis: 'readonly',
                console: 'readonly',
                TextDecoder: 'readonly',
                TextEncoder: 'readonly',
                setTimeout: 'readonly',
                clearTimeout: 'readonly',
                setInterval: 'readonly',
                clearInterval: 'readonly',
            },
        },
        rules: {
            'no-var': 'error',
            'no-eval': 'error',
            'no-implied-eval': 'error',
            'no-else-return': 'error',
            'no-lonely-if': 'error',
            'no-duplicate-imports': 'error',
            'no-unused-vars': ['error', { argsIgnorePattern: '^_' }],
            'prefer-const': 'error',
            eqeqeq: 'error',
            'dot-notation': 'error',

            // Complexity limits (the Rust half enforces a clippy
            // cognitive-complexity threshold; these are the JS equivalent).
            complexity: ['error', 15],
            'max-depth': ['error', 4],
            'max-params': ['error', 5],
            'max-nested-callbacks': ['error', 4],
            'max-lines-per-function': [
                'error',
                { max: 100, skipBlankLines: true, skipComments: true },
            ],
            'max-lines': ['error', { max: 500, skipBlankLines: true, skipComments: true }],

            // Correctness
            'no-shadow': 'error',
            'consistent-return': 'error',
            'array-callback-return': 'error',
            curly: ['error', 'multi-line'],
            'no-implicit-coercion': 'error',
            'no-throw-literal': 'error',
            'no-return-assign': 'error',
            'prefer-arrow-callback': 'error',
            'prefer-template': 'error',
            'object-shorthand': 'error',
            'no-unneeded-ternary': 'error',
            'no-useless-concat': 'error',
            'no-useless-return': 'error',
            'default-case-last': 'error',
            'no-new-wrappers': 'error',
            'no-self-compare': 'error',
            'no-template-curly-in-string': 'error',
        },
    },
    // Must come last: disables any ESLint rules that would fight Prettier.
    prettierConfig,
];
