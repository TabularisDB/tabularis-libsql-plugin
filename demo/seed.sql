-- Demo schema + data for the Tabularis libSQL plugin.
--
-- A tiny blog: authors write posts, posts have comments and tags
-- (many-to-many via post_tags), plus a view over published posts.
-- Foreign keys, a junction table, indexes and a view exercise the
-- plugin's metadata, ER-diagram and view features end to end.

PRAGMA foreign_keys = ON;

CREATE TABLE authors (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    name    TEXT    NOT NULL,
    email   TEXT    UNIQUE,
    country TEXT
);

CREATE TABLE posts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    author_id  INTEGER NOT NULL REFERENCES authors(id) ON DELETE CASCADE,
    title      TEXT    NOT NULL,
    body       TEXT,
    published  INTEGER NOT NULL DEFAULT 0,
    views      INTEGER NOT NULL DEFAULT 0,
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE comments (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    post_id     INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    author_name TEXT    NOT NULL,
    body        TEXT    NOT NULL,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE tags (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT    NOT NULL UNIQUE
);

CREATE TABLE post_tags (
    post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags(id)  ON DELETE CASCADE,
    PRIMARY KEY (post_id, tag_id)
);

CREATE INDEX idx_posts_author   ON posts(author_id);
CREATE INDEX idx_comments_post  ON comments(post_id);
CREATE INDEX idx_post_tags_tag  ON post_tags(tag_id);

CREATE VIEW published_posts AS
    SELECT p.id, p.title, a.name AS author, p.views
    FROM posts p
    JOIN authors a ON a.id = p.author_id
    WHERE p.published = 1;

INSERT INTO authors (name, email, country) VALUES
    ('Ada Lovelace',   'ada@example.com',   'UK'),
    ('Alan Turing',    'alan@example.com',  'UK'),
    ('Grace Hopper',   'grace@example.com', 'US'),
    ('Andrea Debernardi', NULL,             'IT');

INSERT INTO posts (author_id, title, body, published, views) VALUES
    (1, 'On Analytical Engines', 'The engine weaves algebraic patterns…', 1, 1280),
    (1, 'Notes on Notation',     'A draft about a better notation.',       0,    0),
    (2, 'Can Machines Think?',   'I propose to consider the question…',    1, 9001),
    (3, 'Debugging the Mark II', 'Found an actual bug in the relay.',      1,  430),
    (4, 'Shipping libSQL',       'A dual local/remote driver in Rust.',    0,   12);

INSERT INTO comments (post_id, author_name, body) VALUES
    (1, 'Charles', 'Brilliant work.'),
    (1, 'Reader',  'Loved the imagery.'),
    (3, 'Visitor', 'Still relevant today.'),
    (4, 'Intern',  'So that is where the term comes from!');

INSERT INTO tags (name) VALUES
    ('history'), ('computing'), ('ai'), ('rust'), ('databases');

INSERT INTO post_tags (post_id, tag_id) VALUES
    (1, 1), (1, 2),
    (3, 2), (3, 3),
    (4, 1), (4, 2),
    (5, 4), (5, 5);
