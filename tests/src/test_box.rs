use rand::Rng;

use tarantool::index::IteratorType;
use tarantool::sequence::Sequence;
use tarantool::space;
use tarantool::space::{Space, SystemSpace, CreateSpaceOptions};
use tarantool::tuple::Tuple;

use crate::common::{QueryOperation, S1Record, S2Key, S2Record};

pub fn test_space_get_by_name() {
    assert!(Space::find("test_s1").is_some());
    assert!(Space::find("test_s1_invalid").is_none());
}

pub fn test_space_get_system() {
    let space: Space = SystemSpace::Space.into();
    assert!(space.len().is_ok());
}

pub fn test_index_get_by_name() {
    let space = Space::find("test_s2").unwrap();
    assert!(space.index("idx_1").is_some());
    assert!(space.index("idx_1_invalid").is_none());
}

pub fn test_box_get() {
    let space = Space::find("test_s2").unwrap();

    let idx_1 = space.index("idx_1").unwrap();
    let output = idx_1.get(&("key_16".to_string(),)).unwrap();
    assert!(output.is_some());
    assert_eq!(
        output.unwrap().into_struct::<S2Record>().unwrap(),
        S2Record {
            id: 16,
            key: "key_16".to_string(),
            value: "value_16".to_string(),
            a: 1,
            b: 3
        }
    );

    let idx_2 = space.index("idx_2").unwrap();
    let output = idx_2.get(&S2Key { id: 17, a: 2, b: 3 }).unwrap();
    assert!(output.is_some());
    assert_eq!(
        output.unwrap().into_struct::<S2Record>().unwrap(),
        S2Record {
            id: 17,
            key: "key_17".to_string(),
            value: "value_17".to_string(),
            a: 2,
            b: 3
        }
    );
}

pub fn test_box_select() {
    let space = Space::find("test_s2").unwrap();
    let result: Vec<S1Record> = space
        .primary_key()
        .select(IteratorType::LE, &(5,))
        .unwrap()
        .map(|x| x.into_struct().unwrap())
        .collect();
    assert_eq!(
        result,
        vec![
            S1Record {
                id: 5,
                text: "key_5".to_string()
            },
            S1Record {
                id: 4,
                text: "key_4".to_string()
            },
            S1Record {
                id: 3,
                text: "key_3".to_string()
            },
            S1Record {
                id: 2,
                text: "key_2".to_string()
            },
            S1Record {
                id: 1,
                text: "key_1".to_string()
            }
        ]
    );

    let idx = space.index("idx_3").unwrap();
    let result: Vec<S2Record> = idx
        .select(IteratorType::Eq, &(3,))
        .unwrap()
        .map(|x| x.into_struct().unwrap())
        .collect();
    assert_eq!(
        result,
        vec![
            S2Record {
                id: 3,
                key: "key_3".to_string(),
                value: "value_3".to_string(),
                a: 3,
                b: 0
            },
            S2Record {
                id: 8,
                key: "key_8".to_string(),
                value: "value_8".to_string(),
                a: 3,
                b: 1
            },
            S2Record {
                id: 13,
                key: "key_13".to_string(),
                value: "value_13".to_string(),
                a: 3,
                b: 2
            },
            S2Record {
                id: 18,
                key: "key_18".to_string(),
                value: "value_18".to_string(),
                a: 3,
                b: 3
            },
        ]
    );
}

pub fn test_box_select_composite_key() {
    let space = Space::find("test_s2").unwrap();
    let idx = space.index("idx_2").unwrap();

    let result: Vec<S2Record> = idx
        .select(IteratorType::Eq, &(3, 3, 0))
        .unwrap()
        .map(|x| x.into_struct().unwrap())
        .collect();
    assert_eq!(
        result,
        vec![S2Record {
            id: 3,
            key: "key_3".to_string(),
            value: "value_3".to_string(),
            a: 3,
            b: 0
        }]
    );
}

pub fn test_box_len() {
    let space = Space::find("test_s2").unwrap();
    assert_eq!(space.len().unwrap(), 20 as usize);
}

pub fn test_box_random() {
    let space = Space::find("test_s2").unwrap();
    let idx = space.primary_key();
    let mut rng = rand::thread_rng();

    let result = idx.random(rng.gen()).unwrap();
    assert!(result.is_some());

    let output = result.unwrap().into_struct::<S2Record>().unwrap();
    assert_eq!(output.a, (output.id as i32) % 5);
    assert_eq!(output.b, (output.id as i32) / 5);
    assert_eq!(output.key, format!("key_{}", output.id));
    assert_eq!(output.value, format!("value_{}", output.id));
}

pub fn test_box_min_max() {
    let space = Space::find("test_s2").unwrap();
    let idx = space.index("idx_3").unwrap();

    let result_min = idx.min(&(3,)).unwrap();
    assert!(result_min.is_some());
    assert_eq!(
        result_min.unwrap().into_struct::<S2Record>().unwrap(),
        S2Record {
            id: 3,
            key: "key_3".to_string(),
            value: "value_3".to_string(),
            a: 3,
            b: 0
        },
    );

    let result_max = idx.max(&(3,)).unwrap();
    assert!(result_max.is_some());
    assert_eq!(
        result_max.unwrap().into_struct::<S2Record>().unwrap(),
        S2Record {
            id: 18,
            key: "key_18".to_string(),
            value: "value_18".to_string(),
            a: 3,
            b: 3
        },
    );
}

pub fn test_box_count() {
    let space = Space::find("test_s2").unwrap();
    assert_eq!(
        space.primary_key().count(IteratorType::LE, &(7,),).unwrap(),
        7 as usize
    );
    assert_eq!(
        space.primary_key().count(IteratorType::GT, &(7,),).unwrap(),
        13 as usize
    );
}

pub fn test_box_extract_key() {
    let space = Space::find("test_s2").unwrap();
    let idx = space.index("idx_2").unwrap();
    let record = S2Record {
        id: 11,
        key: "key_11".to_string(),
        value: "value_11".to_string(),
        a: 1,
        b: 2,
    };
    assert_eq!(
        idx.extract_key(Tuple::from_struct(&record).unwrap())
            .into_struct::<S2Key>()
            .unwrap(),
        S2Key { id: 11, a: 1, b: 2 }
    );
}

pub fn test_box_insert() {
    let mut space = Space::find("test_s1").unwrap();
    space.truncate().unwrap();

    let input = S1Record {
        id: 1,
        text: "Test".to_string(),
    };
    let insert_result = space.insert(&input).unwrap();
    assert!(insert_result.is_some());
    assert_eq!(
        insert_result.unwrap().into_struct::<S1Record>().unwrap(),
        input
    );

    let output = space.get(&(input.id,)).unwrap();
    assert!(output.is_some());
    assert_eq!(output.unwrap().into_struct::<S1Record>().unwrap(), input);
}

pub fn test_box_replace() {
    let mut space = Space::find("test_s1").unwrap();
    space.truncate().unwrap();

    let original_input = S1Record {
        id: 1,
        text: "Original".to_string(),
    };
    space.insert(&original_input).unwrap();

    let new_input = S1Record {
        id: original_input.id,
        text: "New".to_string(),
    };
    let replace_result = space.replace(&new_input).unwrap();
    assert!(replace_result.is_some());
    assert_eq!(
        replace_result.unwrap().into_struct::<S1Record>().unwrap(),
        new_input
    );

    let output = space.get(&(new_input.id,)).unwrap();
    assert!(output.is_some());
    assert_eq!(
        output.unwrap().into_struct::<S1Record>().unwrap(),
        new_input
    );
}

pub fn test_box_delete() {
    let mut space = Space::find("test_s1").unwrap();
    space.truncate().unwrap();

    let input = S1Record {
        id: 1,
        text: "Test".to_string(),
    };
    space.insert(&input).unwrap();

    let delete_result = space.delete(&(input.id,)).unwrap();
    assert!(delete_result.is_some());
    assert_eq!(
        delete_result.unwrap().into_struct::<S1Record>().unwrap(),
        input
    );

    let output = space.get(&(input.id,)).unwrap();
    assert!(output.is_none());
}

pub fn test_box_update() {
    let mut space = Space::find("test_s1").unwrap();
    space.truncate().unwrap();

    let input = S1Record {
        id: 1,
        text: "Original".to_string(),
    };
    space.insert(&input).unwrap();

    let update_result = space
        .update(
            &(input.id,),
            &vec![QueryOperation {
                op: "=".to_string(),
                field_id: 1,
                value: "New".into(),
            }],
        )
        .unwrap();
    assert!(update_result.is_some());
    assert_eq!(
        update_result
            .unwrap()
            .into_struct::<S1Record>()
            .unwrap()
            .text,
        "New"
    );

    let output = space.get(&(input.id,)).unwrap();
    assert_eq!(
        output.unwrap().into_struct::<S1Record>().unwrap().text,
        "New"
    );
}

pub fn test_box_upsert() {
    let mut space = Space::find("test_s1").unwrap();
    space.truncate().unwrap();

    let original_input = S1Record {
        id: 1,
        text: "Original".to_string(),
    };
    space.insert(&original_input).unwrap();

    space
        .upsert(
            &S1Record {
                id: 1,
                text: "New".to_string(),
            },
            &vec![QueryOperation {
                op: "=".to_string(),
                field_id: 1,
                value: "Test 1".into(),
            }],
        )
        .unwrap();

    space
        .upsert(
            &S1Record {
                id: 2,
                text: "New".to_string(),
            },
            &vec![QueryOperation {
                op: "=".to_string(),
                field_id: 1,
                value: "Test 2".into(),
            }],
        )
        .unwrap();

    let output = space.get(&(1,)).unwrap();
    assert_eq!(
        output.unwrap().into_struct::<S1Record>().unwrap().text,
        "Test 1"
    );

    let output = space.get(&(2,)).unwrap();
    assert_eq!(
        output.unwrap().into_struct::<S1Record>().unwrap().text,
        "New"
    );
}

pub fn test_box_truncate() {
    let mut space = Space::find("test_s1").unwrap();
    space.truncate().unwrap();

    assert_eq!(space.len().unwrap(), 0 as usize);
    for i in 0..10 {
        space
            .insert(&S1Record {
                id: i,
                text: "Test".to_string(),
            })
            .unwrap();
    }
    assert_eq!(space.len().unwrap(), 10 as usize);
    space.truncate().unwrap();
    assert_eq!(space.len().unwrap(), 0 as usize);
}

pub fn test_box_sequence_get_by_name() {
    assert!(Sequence::find("test_seq").unwrap().is_some());
    assert!(Sequence::find("test_seq_invalid").unwrap().is_none());
}

pub fn test_box_sequence_iterate() {
    let mut seq = Sequence::find("test_seq").unwrap().unwrap();
    seq.reset().unwrap();
    assert_eq!(seq.next().unwrap(), 1);
    assert_eq!(seq.next().unwrap(), 2);
}

pub fn test_box_sequence_set() {
    let mut seq = Sequence::find("test_seq").unwrap().unwrap();
    seq.reset().unwrap();
    assert_eq!(seq.next().unwrap(), 1);

    seq.set(99).unwrap();
    assert_eq!(seq.next().unwrap(), 100);
}

pub fn test_create_space() {
    let mut opts = CreateSpaceOptions {
        if_not_exists: false,
        engine: "memtx".to_string(),
        id: 0,
        field_count: 0,
        user: "".to_string(),
        is_local: true,
        temporary: true,
        is_sync: false,
    };

    // Create space with default options.
    let result_1 = space::create_space("new_space_1", &opts);
    // !!!
    if result_1.is_err() {panic!(result_1.err().unwrap())};
    assert_eq!(result_1.is_ok(), true);
    assert_eq!(result_1.unwrap().is_some(), true);

    // Test `SpaceExists` error.
    let result_2 = space::create_space("new_space_1", &opts);
    assert_eq!(result_2.is_err(), true);

    // Test `if_not_exists` option.
    opts.if_not_exists = true;
    let result_3 = space::create_space("new_space_1", &opts);
    // !!!
    if result_3.is_err() {panic!(result_3.err().unwrap())};
    assert_eq!(result_3.is_err(), false);
    assert_eq!(result_3.unwrap().is_none(), true);
    opts.if_not_exists = false;

    // Test `id` option.
    // opts.id = 10000;
    // let result_4 = space::create_space("new_space_2", &opts);
    // let id = result_4.unwrap().unwrap().id();
    // assert_eq!(id, opts.id);
    // opts.id = 0;

    // Test correct ID increment of created spaces.
    let mut prev_id = Space::find("new_space_1").unwrap().id();
    for i in 2..6 {
        let space_name = format!("new_space_{}", i);
        let result = space::create_space(space_name.as_str(), &opts);
        // !!!
        if result.is_err() {panic!(result.err().unwrap())};
        let curr_id = result.unwrap().unwrap().id();
        assert_eq!(prev_id+1, curr_id);
        prev_id = curr_id;
    }

    // Test `user` option and `NoSuchUser` error.
    opts.user = "admin".to_string();
    let result_4 = space::create_space("new_space_6", &opts);
    // !!!
    if result_4.is_err() {panic!(result_4.err().unwrap())};
    assert_eq!(result_4.is_ok(), true);
    assert_eq!(result_4.unwrap().is_some(), true);

    opts.user = "user".to_string();
    let result_5 = space::create_space("new_space_7", &opts);
    assert_eq!(result_5.is_err(), true);

    // Test `is_local` and `temporary` options.
}
